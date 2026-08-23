use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::{
    sync::{Mutex, RwLock},
    task::JoinHandle,
};

use crate::{
    config::DatabaseConfig,
    error::AppError,
    values::{MAX_KEY_BYTES, validate_table},
};

const DUPLICATE_TABLE_SQLSTATE: &str = "42P07";
const MAX_LIST_ROWS: i64 = 10_000;
/// Shards for the per-key write locks. Same-key writers serialize on one
/// shard; different keys rarely collide (collision only costs a short queue).
const MUTEX_SHARDS: usize = 256;

#[derive(Debug)]
struct MemEntry {
    value: Value,
    expires_at: Option<Instant>,
}

impl MemEntry {
    fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
    }
}

/// Tracks which writes still need to be pushed to `PostgreSQL`.
#[derive(Debug, Default)]
struct DirtyTracker {
    /// Keys written since the last flush (upsert on flush).
    upserts: HashSet<(String, String)>,
    /// Keys deleted since the last flush (delete on flush).
    deletes: HashSet<(String, String)>,
    /// Tables fully cleared since the last flush (wipe on flush).
    cleared: HashSet<String>,
}

impl DirtyTracker {
    fn mark_upsert(&mut self, table: &str, key: &str) {
        self.upserts.insert((table.to_owned(), key.to_owned()));
        self.deletes.remove(&(table.to_owned(), key.to_owned()));
    }

    fn mark_delete(&mut self, table: &str, key: &str) {
        self.deletes.insert((table.to_owned(), key.to_owned()));
        self.upserts.remove(&(table.to_owned(), key.to_owned()));
    }

    fn clear_table(&mut self, table: &str) {
        self.cleared.insert(table.to_owned());
        self.upserts.retain(|(t, _)| t != table);
        self.deletes.retain(|(t, _)| t != table);
    }
}

#[derive(Clone)]
pub struct Repository {
    pool: PgPool,
    ensured_tables: Arc<RwLock<HashSet<String>>>,
    /// Write-behind store: table -> key -> entry. Writes land here first and
    /// are batch-flushed to `PostgreSQL` every `flush_interval`.
    memory: Arc<RwLock<HashMap<String, HashMap<String, MemEntry>>>>,
    dirty: Arc<Mutex<DirtyTracker>>,
    flush_interval: Duration,
    flush_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    write_locks: Arc<[Mutex<()>; MUTEX_SHARDS]>,
    memory_hits: Arc<AtomicU64>,
    memory_misses: Arc<AtomicU64>,
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

        let repository = Self {
            pool,
            ensured_tables: Arc::new(RwLock::new(HashSet::new())),
            memory: Arc::new(RwLock::new(HashMap::new())),
            dirty: Arc::new(Mutex::new(DirtyTracker::default())),
            flush_interval: Duration::from_secs(config.flush_interval_secs),
            flush_task: Arc::new(Mutex::new(None)),
            write_locks: Arc::new(std::array::from_fn(|_| Mutex::new(()))),
            memory_hits: Arc::new(AtomicU64::new(0)),
            memory_misses: Arc::new(AtomicU64::new(0)),
        };
        let flush_interval = repository.flush_interval;
        let task = tokio::spawn(repository.clone().flush_loop(flush_interval));
        *repository.flush_task.lock().await = Some(task);
        Ok(repository)
    }

    pub async fn get(&self, table: &str, key: &str) -> Result<Option<Value>, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.get");
        self.read_inner(table, key).await
    }

    pub async fn set(&self, table: &str, key: &str, value: &Value) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.set");
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        self.write_memory(table, key, value.clone(), None).await;
        self.mark_upsert(table, key).await;
        Ok(())
    }

    pub async fn set_with_ttl(
        &self,
        table: &str,
        key: &str,
        value: &Value,
        ttl: Duration,
    ) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, ttl = ?ttl, "repository.set_with_ttl");
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        self.write_memory(table, key, value.clone(), Some(Instant::now() + ttl))
            .await;
        self.mark_upsert(table, key).await;
        Ok(())
    }

    pub async fn delete(&self, table: &str, key: &str) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.delete");
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        self.remove_memory(table, key).await;
        self.mark_delete(table, key).await;
        Ok(())
    }

    pub async fn list_all(&self, table: &str) -> Result<Vec<(String, Value)>, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, "repository.list_all");

        let mut merged: HashMap<String, Value> = HashMap::new();
        let tables = self.memory.read().await;
        let cleared = self.dirty.lock().await.cleared.contains(table);

        if !cleared {
            let select = format!(r#"SELECT id, value FROM "{table}" ORDER BY id LIMIT $1"#);
            let rows = sqlx::query(&select)
                .bind(MAX_LIST_ROWS)
                .fetch_all(&self.pool)
                .await?;
            for row in rows {
                let id: String = row.try_get("id")?;
                let raw: String = row.try_get("value")?;
                let value: Value = serde_json::from_str(&raw)?;
                merged.insert(id, value);
            }
        }

        let (upserts, deletes) = {
            let dirty = self.dirty.lock().await;
            let upserts: HashSet<(String, String)> = dirty
                .upserts
                .iter()
                .filter(|(t, _)| t == table)
                .cloned()
                .collect();
            let deletes: HashSet<(String, String)> = dirty
                .deletes
                .iter()
                .filter(|(t, _)| t == table)
                .cloned()
                .collect();
            (upserts, deletes)
        };
        for (_, key) in &deletes {
            merged.remove(key);
        }
        if let Some(entries) = tables.get(table) {
            for (key, entry) in entries {
                if entry.is_expired() {
                    continue;
                }
                if upserts.contains(&(table.to_owned(), key.clone())) {
                    merged.insert(key.clone(), entry.value.clone());
                }
            }
        }
        drop(tables);

        let mut rows: Vec<(String, Value)> = merged.into_iter().collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows.truncate(usize::try_from(MAX_LIST_ROWS).unwrap_or(usize::MAX));
        Ok(rows)
    }

    pub async fn clear_table(&self, table: &str) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, "repository.clear_table");
        self.memory.write().await.remove(table);
        self.dirty.lock().await.clear_table(table);
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
        tracing::trace!(table, key, "repository.mutate");

        // The per-key lock makes the read-modify-write atomic against every
        // other write on the same key. PostgreSQL is only touched by the
        // periodic batch flush, so no row locks or CAS retries are needed.
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        let current = self.read_inner(table, key).await?.unwrap_or(absent);
        if let Some(updated) = transform(current)? {
            self.write_memory(table, key, updated.clone(), None).await;
            self.mark_upsert(table, key).await;
            Ok(updated)
        } else {
            self.remove_memory(table, key).await;
            self.mark_delete(table, key).await;
            Ok(Value::Null)
        }
    }

    /// Removes expired TTL entries; returns how many were purged.
    pub async fn purge_expired(&self) -> usize {
        let expired: Vec<(String, String)> = {
            let mut tables = self.memory.write().await;
            let mut expired = Vec::new();
            for (table, entries) in tables.iter_mut() {
                entries.retain(|key, entry| {
                    if entry.is_expired() {
                        expired.push((table.clone(), key.clone()));
                        false
                    } else {
                        true
                    }
                });
            }
            tables.retain(|_, entries| !entries.is_empty());
            expired
        };
        for (table, key) in &expired {
            self.mark_delete(table, key).await;
        }
        expired.len()
    }

    /// Batch-pushes all buffered writes to `PostgreSQL`. Safe to call
    /// concurrently: entries written during the flush stay dirty for the next
    /// cycle, and on failure everything is retried on the next cycle.
    pub async fn flush(&self) -> Result<(), AppError> {
        let (cleared, upserts, deletes) = {
            let dirty = self.dirty.lock().await;
            (
                dirty.cleared.iter().cloned().collect::<HashSet<_>>(),
                dirty.upserts.iter().cloned().collect::<HashSet<_>>(),
                dirty.deletes.iter().cloned().collect::<HashSet<_>>(),
            )
        };
        if cleared.is_empty() && upserts.is_empty() && deletes.is_empty() {
            return Ok(());
        }
        let started = Instant::now();

        // 1) Fully cleared tables first.
        for table in &cleared {
            let delete = format!(r#"DELETE FROM "{table}""#);
            sqlx::query(&delete).execute(&self.pool).await?;
        }

        // 2) Key deletions, batched per table.
        let mut deletes_by_table: HashMap<String, Vec<String>> = HashMap::new();
        for (table, key) in &deletes {
            if cleared.contains(table) {
                continue;
            }
            deletes_by_table
                .entry(table.clone())
                .or_default()
                .push(key.clone());
        }
        for (table, keys) in &deletes_by_table {
            let delete = format!(r#"DELETE FROM "{table}" WHERE id = ANY($1::text[])"#);
            sqlx::query(&delete).bind(keys).execute(&self.pool).await?;
        }

        // 3) Upserts: read the freshest in-memory value for each dirty key.
        let mut upserts_by_table: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (table, key) in &upserts {
            let value = {
                let tables = self.memory.read().await;
                tables
                    .get(table)
                    .and_then(|entries| entries.get(key))
                    .filter(|entry| !entry.is_expired())
                    .map(|entry| entry.value.clone())
            };
            let Some(value) = value else {
                // Deleted, expired, or purged before the flush landed; make
                // sure the stale row (if any) is removed from `PostgreSQL`.
                self.mark_delete(table, key).await;
                continue;
            };
            let payload = serde_json::to_string(&value)?;
            upserts_by_table
                .entry(table.clone())
                .or_default()
                .push((key.clone(), payload));
        }
        for (table, rows) in &upserts_by_table {
            self.ensure_table(table).await?;
            let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
            let payloads: Vec<String> = rows.iter().map(|(_, payload)| payload.clone()).collect();
            let insert = format!(
                r#"INSERT INTO "{table}" (id, value) SELECT * FROM UNNEST($1::text[], $2::text[]) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value"#
            );
            sqlx::query(&insert)
                .bind(&ids)
                .bind(&payloads)
                .execute(&self.pool)
                .await?;
        }

        // Only drop what we actually flushed; writes that landed meanwhile
        // stay dirty for the next cycle.
        let mut dirty = self.dirty.lock().await;
        dirty.cleared.retain(|table| !cleared.contains(table));
        dirty.upserts.retain(|key| !upserts.contains(key));
        dirty.deletes.retain(|key| !deletes.contains(key));

        tracing::info!(
            cleared = cleared.len(),
            deletes = deletes.len(),
            upserts = upserts.len(),
            elapsed = ?started.elapsed(),
            "repository flush complete"
        );
        Ok(())
    }

    /// Stops the background flush loop and pushes any remaining writes.
    pub async fn shutdown(&self) {
        if let Some(task) = self.flush_task.lock().await.take() {
            task.abort();
        }
        if let Err(error) = self.flush().await {
            tracing::error!(%error, "final repository flush failed");
        }
        self.pool.close().await;
    }

    pub fn memory_stats(&self) -> (u64, u64) {
        (
            self.memory_hits.load(Ordering::Relaxed),
            self.memory_misses.load(Ordering::Relaxed),
        )
    }

    async fn flush_loop(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            if let Err(error) = self.flush().await {
                tracing::error!(%error, "repository flush failed, will retry next cycle");
            }
        }
    }

    /// Reads a key from memory, falling back to `PostgreSQL` for keys that were
    /// never touched since startup. Expired TTL entries are treated as absent
    /// (`PostgreSQL` may still hold them until the next flush deletes them).
    async fn read_inner(&self, table: &str, key: &str) -> Result<Option<Value>, AppError> {
        {
            let mut tables = self.memory.write().await;
            if let Some(entry) = tables
                .get_mut(table)
                .and_then(|entries| entries.get_mut(key))
            {
                self.memory_hits.fetch_add(1, Ordering::Relaxed);
                if !entry.is_expired() {
                    return Ok(Some(entry.value.clone()));
                }
                // Expired: drop it and treat the key as absent.
                if let Some(entries) = tables.get_mut(table) {
                    entries.remove(key);
                }
                self.mark_delete(table, key).await;
                return Ok(None);
            }
        }
        self.memory_misses.fetch_add(1, Ordering::Relaxed);

        // The key may be logically absent: deleted or expired since the last
        // flush, or its table was cleared. `PostgreSQL` may still hold the
        // old row until the next flush deletes it, so it must not be
        // resurrected here.
        let key_tuple = (table.to_owned(), key.to_owned());
        {
            let dirty = self.dirty.lock().await;
            if dirty.deletes.contains(&key_tuple) || dirty.cleared.contains(table) {
                return Ok(None);
            }
        }

        let select = format!(r#"SELECT value FROM "{table}" WHERE id = $1"#);
        let row = sqlx::query(&select)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        let Some(value) = row.as_ref().map(parse_row_value).transpose()? else {
            return Ok(None);
        };

        // The key may have been written to memory while we were reading
        // PostgreSQL; the fresher in-memory value wins.
        let mut tables = self.memory.write().await;
        let entries = tables.entry(table.to_owned()).or_default();
        let current = entries.entry(key.to_owned()).or_insert_with(|| MemEntry {
            value: value.clone(),
            expires_at: None,
        });
        Ok(Some(current.value.clone()))
    }

    async fn write_memory(
        &self,
        table: &str,
        key: &str,
        value: Value,
        expires_at: Option<Instant>,
    ) {
        let mut tables = self.memory.write().await;
        tables
            .entry(table.to_owned())
            .or_default()
            .insert(key.to_owned(), MemEntry { value, expires_at });
    }

    async fn remove_memory(&self, table: &str, key: &str) {
        let mut tables = self.memory.write().await;
        if let Some(entries) = tables.get_mut(table) {
            entries.remove(key);
        }
    }

    async fn mark_upsert(&self, table: &str, key: &str) {
        self.dirty.lock().await.mark_upsert(table, key);
    }

    async fn mark_delete(&self, table: &str, key: &str) {
        self.dirty.lock().await.mark_delete(table, key);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tracker_tracks_upserts_deletes_and_table_clears() {
        let mut tracker = DirtyTracker::default();

        tracker.mark_upsert("t", "a");
        tracker.mark_upsert("t", "b");
        assert!(tracker.upserts.contains(&("t".to_owned(), "a".to_owned())));

        tracker.mark_delete("t", "a");
        assert!(!tracker.upserts.contains(&("t".to_owned(), "a".to_owned())));
        assert!(tracker.deletes.contains(&("t".to_owned(), "a".to_owned())));
        assert!(tracker.upserts.contains(&("t".to_owned(), "b".to_owned())));

        tracker.mark_upsert("t", "a");
        assert!(!tracker.deletes.contains(&("t".to_owned(), "a".to_owned())));
        assert!(tracker.upserts.contains(&("t".to_owned(), "a".to_owned())));

        tracker.clear_table("t");
        assert!(tracker.cleared.contains("t"));
        assert!(tracker.upserts.is_empty());
        assert!(tracker.deletes.is_empty());
    }

    #[test]
    fn mem_entry_expires_only_with_elapsed_ttl() {
        let fresh = MemEntry {
            value: json!(1),
            expires_at: Some(Instant::now() + Duration::from_mins(1)),
        };
        assert!(!fresh.is_expired());

        let expired = MemEntry {
            value: json!(1),
            expires_at: Some(Instant::now()),
        };
        assert!(expired.is_expired());

        let never = MemEntry {
            value: json!(1),
            expires_at: None,
        };
        assert!(!never.is_expired());
    }
}
