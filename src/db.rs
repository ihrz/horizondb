use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::sync::RwLock;

use crate::{
    config::DatabaseConfig,
    error::AppError,
    values::{MAX_KEY_BYTES, validate_table},
};

const DUPLICATE_TABLE_SQLSTATE: &str = "42P07";
const MAX_LIST_ROWS: u32 = 10_000;
const MAX_CAS_ATTEMPTS: u32 = 128;

#[derive(Clone)]
pub struct Repository {
    pool: PgPool,
    ensured_tables: Arc<RwLock<HashSet<String>>>,
}

impl Repository {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, AppError> {
        let database_url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            urlencoding::encode(&config.username),
            urlencoding::encode(config.password.expose()),
            config.host,
            config.port,
            config.database,
        );
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(Duration::from_secs(config.acquire_timeout_secs))
            .idle_timeout(Duration::from_secs(config.idle_timeout_secs))
            .max_lifetime(Duration::from_secs(config.max_lifetime_secs))
            .connect(&database_url)
            .await?;
        sqlx::query("SELECT 1").execute(&pool).await?;
        Ok(Self {
            pool,
            ensured_tables: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn get(&self, table: &str, key: &str) -> Result<Option<Value>, AppError> {
        self.ensure_table(table).await?;
        let select = format!(r#"SELECT value FROM "{table}" WHERE id = $1"#);
        let row = sqlx::query(&select)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(parse_row_value).transpose()
    }

    pub async fn set(&self, table: &str, key: &str, value: &Value) -> Result<(), AppError> {
        let started = Instant::now();
        self.ensure_table(table).await?;
        let ensured = started.elapsed();
        let payload = serde_json::to_string(value)?;
        let upsert = format!(
            r#"INSERT INTO "{table}" (id, value) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value"#
        );
        sqlx::query(&upsert)
            .bind(key)
            .bind(payload)
            .execute(&self.pool)
            .await?;
        let total = started.elapsed();
        if total > std::time::Duration::from_millis(2) {
            tracing::warn!(?total, ?ensured, table, key, "slow repository.set");
        }
        Ok(())
    }

    pub async fn delete(&self, table: &str, key: &str) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        let delete = format!(r#"DELETE FROM "{table}" WHERE id = $1"#);
        sqlx::query(&delete).bind(key).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn list_all(&self, table: &str) -> Result<Vec<(String, Value)>, AppError> {
        self.ensure_table(table).await?;
        let select = format!(r#"SELECT id, value FROM "{table}" ORDER BY id LIMIT $1"#);
        let rows = sqlx::query(&select)
            .bind(i64::from(MAX_LIST_ROWS))
            .fetch_all(&self.pool)
            .await?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let raw: String = row.try_get("value")?;
            let value: Value = serde_json::from_str(&raw)?;
            entries.push((id, value));
        }
        Ok(entries)
    }

    pub async fn clear_table(&self, table: &str) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        let delete = format!(r#"DELETE FROM "{table}""#);
        sqlx::query(&delete).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn mutate<F>(
        &self,
        table: &str,
        key: &str,
        absent: Value,
        transform: F,
    ) -> Result<Value, AppError>
    where
        F: Fn(Value) -> Result<Option<Value>, AppError>,
    {
        assert_key_fits(key)?;
        self.ensure_table(table).await?;

        for attempt in 0..MAX_CAS_ATTEMPTS {
            let existing = self.read_row(table, key).await?;
            let previous = existing.clone();
            let current = existing.unwrap_or_else(|| absent.clone());

            if let Some(updated) = transform(current)? {
                let new_payload = serde_json::to_string(&updated)?;
                let old_payload = match &previous {
                    Some(value) => serde_json::to_string(value)?,
                    None => String::new(),
                };
                if previous.is_some() {
                    let update = format!(
                        r#"UPDATE "{table}" SET value = $2 WHERE id = $1 AND value IS NOT DISTINCT FROM $3"#
                    );
                    let result = sqlx::query(&update)
                        .bind(key)
                        .bind(new_payload)
                        .bind(old_payload)
                        .execute(&self.pool)
                        .await?;
                    if result.rows_affected() == 1 {
                        return Ok(updated);
                    }
                } else {
                    let insert = format!(
                        r#"INSERT INTO "{table}" (id, value) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING"#
                    );
                    let result = sqlx::query(&insert)
                        .bind(key)
                        .bind(new_payload)
                        .execute(&self.pool)
                        .await?;
                    if result.rows_affected() == 1 {
                        return Ok(updated);
                    }
                }
            } else {
                let old_payload = match &previous {
                    Some(value) => serde_json::to_string(value)?,
                    None => return Ok(Value::Null),
                };
                let delete = format!(
                    r#"DELETE FROM "{table}" WHERE id = $1 AND value IS NOT DISTINCT FROM $2"#
                );
                sqlx::query(&delete)
                    .bind(key)
                    .bind(old_payload)
                    .execute(&self.pool)
                    .await?;
                return Ok(Value::Null);
            }
            let backoff_ms = std::cmp::min(25, u64::from(attempt) * 2 + 1);
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        }
        Err(AppError::internal("contention limit exceeded"))
    }

    async fn read_row(&self, table: &str, key: &str) -> Result<Option<Value>, AppError> {
        let select = format!(r#"SELECT value FROM "{table}" WHERE id = $1"#);
        let row = sqlx::query(&select)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(parse_row_value).transpose()
    }

    async fn ensure_table(&self, table: &str) -> Result<(), AppError> {
        validate_table(table)?;
        {
            let ensured = self.ensured_tables.read().await;
            if ensured.contains(table) {
                return Ok(());
            }
        }
        let mut ensured = self.ensured_tables.write().await;
        if ensured.contains(table) {
            return Ok(());
        }
        let create = format!(
            r#"CREATE TABLE IF NOT EXISTS "{table}" (id VARCHAR(255) PRIMARY KEY, value TEXT)"#
        );
        match sqlx::query(&create).execute(&self.pool).await {
            Ok(_) => {}
            Err(error) => {
                let tolerated = error.as_database_error().is_some_and(|database_error| {
                    database_error
                        .code()
                        .is_some_and(|code| code == DUPLICATE_TABLE_SQLSTATE)
                        || database_error
                            .message()
                            .contains("pg_type_typname_nsp_index")
                });
                if !tolerated {
                    return Err(AppError::Database(error));
                }
            }
        }
        ensured.insert(table.to_owned());
        Ok(())
    }
}

fn assert_key_fits(key: &str) -> Result<(), AppError> {
    if key.len() > MAX_KEY_BYTES {
        Err(AppError::validation("key too long"))
    } else {
        Ok(())
    }
}

fn parse_row_value(row: &sqlx::postgres::PgRow) -> Result<Value, AppError> {
    let raw: String = row.try_get("value")?;
    Ok(serde_json::from_str(&raw)?)
}
