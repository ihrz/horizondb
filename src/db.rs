use std::{
    collections::HashSet,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::sync::{Mutex, RwLock};

use crate::{
    config::DatabaseConfig,
    error::AppError,
    values::{MAX_KEY_BYTES, validate_table},
};

const DUPLICATE_TABLE_SQLSTATE: &str = "42P07";
const MAX_LIST_ROWS: u32 = 10_000;
const MAX_CAS_ATTEMPTS: u32 = 128;
/// Shards for the per-key mutation locks. Same-key mutates serialize on one
/// shard; different keys rarely collide (collision only costs a short queue).
const MUTEX_SHARDS: usize = 256;

#[derive(Clone)]
pub struct Repository {
    pool: PgPool,
    ensured_tables: Arc<RwLock<HashSet<String>>>,
    mutate_locks: Arc<[Mutex<()>; MUTEX_SHARDS]>,
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
            mutate_locks: Arc::new(std::array::from_fn(|_| Mutex::new(()))),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn get(&self, table: &str, key: &str) -> Result<Option<Value>, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.get");
        let select = format!(r#"SELECT value FROM "{table}" WHERE id = $1"#);
        let row = sqlx::query(&select)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(parse_row_value).transpose()
    }

    pub async fn set(&self, table: &str, key: &str, value: &Value) -> Result<(), AppError> {
        let started = Instant::now();
        tracing::trace!(table, key, "repository.set");
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
        tracing::trace!(table, key, "repository.delete");
        let delete = format!(r#"DELETE FROM "{table}" WHERE id = $1"#);
        sqlx::query(&delete).bind(key).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn list_all(&self, table: &str) -> Result<Vec<(String, Value)>, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, "repository.list_all");
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
        tracing::trace!(table, "repository.clear_table");
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

        // Serialize read-modify-writes per key: an in-process sharded mutex
        // prevents concurrent mutates on the same key from racing each other,
        // and the transaction's `SELECT ... FOR UPDATE` makes other writers
        // (plain set/delete, other server processes) queue on the row lock
        // instead of spinning. The compare-and-swap guard compares values
        // semantically via `::jsonb` because rows written by other clients
        // (e.g. the bot's own Postgres driver) may order JSON keys or format
        // numbers differently than serde_json's canonical output. The retry
        // loop below is only a safety net for the narrow cross-process
        // insert race.
        let _guard = self.mutate_locks[mutate_shard(table, key)].lock().await;

        for attempt in 0..MAX_CAS_ATTEMPTS {
            let mut tx = self.pool.begin().await?;
            let existing = Self::read_row_locked(&mut tx, table, key).await?;
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
                        r#"UPDATE "{table}" SET value = $2 WHERE id = $1 AND value::jsonb IS NOT DISTINCT FROM $3::jsonb"#
                    );
                    let result = sqlx::query(&update)
                        .bind(key)
                        .bind(new_payload)
                        .bind(old_payload)
                        .execute(&mut *tx)
                        .await?;
                    if result.rows_affected() == 1 {
                        tx.commit().await?;
                        tracing::trace!(table, key, attempt, "mutate updated");
                        return Ok(updated);
                    }
                    tracing::debug!(
                        table,
                        key,
                        attempt,
                        "mutate update matched 0 rows, retrying"
                    );
                } else {
                    let insert = format!(
                        r#"INSERT INTO "{table}" (id, value) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING"#
                    );
                    let result = sqlx::query(&insert)
                        .bind(key)
                        .bind(new_payload)
                        .execute(&mut *tx)
                        .await?;
                    if result.rows_affected() == 1 {
                        tx.commit().await?;
                        tracing::trace!(table, key, attempt, "mutate inserted");
                        return Ok(updated);
                    }
                    tracing::debug!(table, key, attempt, "mutate insert conflicted, retrying");
                }
            } else {
                let old_payload = if let Some(value) = &previous {
                    serde_json::to_string(value)?
                } else {
                    tx.rollback().await?;
                    return Ok(Value::Null);
                };
                let delete = format!(
                    r#"DELETE FROM "{table}" WHERE id = $1 AND value::jsonb IS NOT DISTINCT FROM $2::jsonb"#
                );
                sqlx::query(&delete)
                    .bind(key)
                    .bind(old_payload)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                tracing::trace!(table, key, attempt, "mutate deleted");
                return Ok(Value::Null);
            }
            tx.rollback().await?;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        tracing::error!(
            table,
            key,
            attempts = MAX_CAS_ATTEMPTS,
            "contention limit exceeded"
        );
        Err(AppError::internal(format!(
            "contention limit exceeded (table={table:?}, key={key:?})"
        )))
    }

    async fn read_row_locked(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        table: &str,
        key: &str,
    ) -> Result<Option<Value>, AppError> {
        let select = format!(r#"SELECT value FROM "{table}" WHERE id = $1 FOR UPDATE"#);
        let row = sqlx::query(&select)
            .bind(key)
            .fetch_optional(&mut **tx)
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

fn mutate_shard(table: &str, key: &str) -> usize {
    let mut hasher = DefaultHasher::new();
    table.hash(&mut hasher);
    0xff_u8.hash(&mut hasher);
    key.hash(&mut hasher);
    // Shard is < MUTEX_SHARDS (256) so it always fits in a usize.
    usize::try_from(hasher.finish() % MUTEX_SHARDS as u64).unwrap_or(0)
}

fn parse_row_value(row: &sqlx::postgres::PgRow) -> Result<Value, AppError> {
    let raw: String = row.try_get("value")?;
    Ok(serde_json::from_str(&raw)?)
}
