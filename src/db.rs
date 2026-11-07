use std::{
    collections::{HashMap, HashSet, hash_map::Entry as MapSlot},
    hash::{DefaultHasher, Hash, Hasher},
    ops::Deref,
    sync::{
        Arc, LazyLock,
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
/// Catalog index hit by two sessions creating the same table at once.
const TYPE_NAME_INDEX: &str = "pg_type_typname_nsp_index";
const MAX_LIST_ROWS: i64 = 10_000;
/// Shards for the per-key write locks. Same-key writers serialize on one
/// shard; different keys rarely collide (collision only costs a short queue).
const MUTEX_SHARDS: usize = 256;
/// Upper bound of rows per `INSERT ... UNNEST` statement during a flush.
const FLUSH_ROWS_PER_STATEMENT: usize = 500;
/// Soft upper bound of payload bytes per `INSERT` statement during a flush.
const FLUSH_BYTES_PER_STATEMENT: usize = 32 * 1024 * 1024;
/// Upper bound of ids per `DELETE ... = ANY` statement during a flush.
const FLUSH_DELETES_PER_STATEMENT: usize = 5_000;
/// Documents serialized/parsed between two cooperative yields, so that a
/// large flush or table scan cannot monopolize a runtime worker.
const YIELD_EVERY: usize = 32;
/// JSON payloads at or above this size are parsed on the blocking pool.
const BLOCKING_PARSE_THRESHOLD: usize = 256 * 1024;
/// A key map is shrunk after a sweep once it uses less than
/// `1 / SHRINK_RATIO` of its capacity: `HashMap` never gives back the
/// buckets of removed entries on its own.
const SHRINK_RATIO: usize = 4;

/// Reference point of the coarse access clock.
static CLOCK_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Seconds elapsed since the first use of the access clock.
fn now_secs() -> u64 {
    CLOCK_EPOCH.elapsed().as_secs()
}

/// A document is held either parsed (`value`, the hot form every operation
/// works on) or packed as compact JSON text (`packed`, about ten times
/// smaller than the parsed tree: a `serde_json` map node is sized for eleven
/// entries even when it holds three). The sweep packs documents that went
/// idle; the next access parses them back in place.
#[derive(Debug)]
struct MemEntry {
    /// The document while hot. `Value::Null` (no allocation) while packed.
    value: Value,
    packed: Option<Arc<str>>,
    expires_at: Option<Instant>,
}

impl MemEntry {
    fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
    }

    fn replace(&mut self, value: Value) {
        self.value = value;
        self.packed = None;
    }

    /// Whether packing would save memory: scalars are already compact.
    fn is_packable(&self) -> bool {
        self.packed.is_none() && (self.value.is_object() || self.value.is_array())
    }

    /// Swaps the parsed tree for its JSON text. Runs on the blocking pool.
    fn pack(&mut self) -> bool {
        if !self.is_packable() {
            return false;
        }
        match serde_json::to_string(&self.value) {
            Ok(text) => {
                self.packed = Some(text.into());
                self.value = Value::Null;
                true
            }
            Err(_) => false,
        }
    }

    /// Parses a packed document back in place.
    async fn unpack(&mut self) -> Result<(), AppError> {
        if let Some(text) = self.packed.clone() {
            self.value = parse_packed(text).await?;
            self.packed = None;
        }
        Ok(())
    }

    /// The document as JSON text, for a flush.
    fn to_json(&self) -> Result<String, AppError> {
        match &self.packed {
            Some(text) => Ok(text.to_string()),
            None => Ok(serde_json::to_string(&self.value)?),
        }
    }

    /// An owned copy of the document, without unpacking the entry.
    async fn to_value(&self) -> Result<Value, AppError> {
        match &self.packed {
            Some(text) => parse_packed(Arc::clone(text)).await,
            None => Ok(self.value.clone()),
        }
    }
}

/// One stored document. Readers take the inner read lock and copy only the
/// part they need; writers mutate the document in place under the inner
/// write lock. The outer store lock is never held while waiting for an
/// inner lock and an inner lock is never held while taking the outer one.
#[derive(Debug)]
struct Slot {
    state: RwLock<MemEntry>,
    /// Last read or write, in `now_secs`. Kept outside the inner lock so the
    /// sweep can spot idle entries without locking each of them.
    last_access: AtomicU64,
}

impl Slot {
    fn touch(&self) {
        self.last_access.store(now_secs(), Ordering::Relaxed);
    }

    fn idle_for(&self, now: u64) -> u64 {
        now.saturating_sub(self.last_access.load(Ordering::Relaxed))
    }
}

impl Deref for Slot {
    type Target = RwLock<MemEntry>;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

type Entry = Arc<Slot>;
type Store = HashMap<String, HashMap<String, Entry>>;
/// Per-key locks for in-progress `PostgreSQL` loads (single-flight).
type LoadLocks = HashMap<(String, String), Arc<Mutex<()>>>;

/// What a mutation does to the TTL of the entry it touches.
#[derive(Clone, Copy, Debug)]
pub enum Expiry {
    /// The entry becomes permanent (regular writes).
    Clear,
    /// The entry expires at the given instant (`cache` operation).
    Set(Instant),
}

fn new_entry(value: Value, expires_at: Option<Instant>) -> Entry {
    Arc::new(Slot {
        state: RwLock::new(MemEntry {
            value,
            packed: None,
            expires_at,
        }),
        last_access: AtomicU64::new(now_secs()),
    })
}

/// What one sweep of the in-memory store did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Expired TTL entries removed (and deleted from `PostgreSQL`).
    pub purged: usize,
    /// Idle, already flushed entries dropped from memory only.
    pub evicted: usize,
    /// Idle entries packed to compact JSON text.
    pub packed: usize,
}

/// When the sweep compacts or drops idle documents, in seconds without any
/// read or write. `None` disables the step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryPolicy {
    pub pack_after: Option<u64>,
    pub evict_after: Option<u64>,
}

impl MemoryPolicy {
    pub fn new(pack_idle_secs: u64, idle_eviction_secs: u64) -> Self {
        Self {
            pack_after: (pack_idle_secs > 0).then_some(pack_idle_secs),
            evict_after: (idle_eviction_secs > 0).then_some(idle_eviction_secs),
        }
    }
}

/// Counters of the in-memory store, for the periodic stats line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub hits: u64,
    pub misses: u64,
    /// Documents held in memory after the last sweep.
    pub entries: u64,
    /// Documents evicted since startup.
    pub evicted: u64,
    /// Documents held packed after the last sweep.
    pub packed: u64,
}

/// Why the sweep picked an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SweepReason {
    Expired,
    Idle,
}

/// Tracks which writes still need to be pushed to `PostgreSQL`.
///
/// Every mark carries the generation it was made in. A flush snapshots the
/// current generation, bumps it, and afterwards only forgets marks that are
/// not newer than the snapshot, so a write that lands while a flush is in
/// progress stays dirty for the next cycle instead of being lost.
#[derive(Debug, Default)]
struct DirtyTracker {
    generation: u64,
    /// Keys written since the last flush (upsert on flush).
    upserts: HashMap<(String, String), u64>,
    /// Keys deleted since the last flush (delete on flush).
    deletes: HashMap<(String, String), u64>,
    /// Tables fully cleared since the last flush (wipe on flush).
    cleared: HashMap<String, u64>,
}

impl DirtyTracker {
    fn mark_upsert(&mut self, table: &str, key: &str) {
        let tuple = (table.to_owned(), key.to_owned());
        self.deletes.remove(&tuple);
        self.upserts.insert(tuple, self.generation);
    }

    fn mark_delete(&mut self, table: &str, key: &str) {
        let tuple = (table.to_owned(), key.to_owned());
        self.upserts.remove(&tuple);
        self.deletes.insert(tuple, self.generation);
    }

    fn clear_table(&mut self, table: &str) {
        self.upserts.retain(|(t, _), _| t != table);
        self.deletes.retain(|(t, _), _| t != table);
        self.cleared.insert(table.to_owned(), self.generation);
    }

    /// Whether memory holds the only up-to-date copy of the key, i.e. it
    /// must stay resident until a flush pushed it to `PostgreSQL`.
    fn is_pending(&self, table: &str, key: &str) -> bool {
        self.cleared.contains_key(table)
            || self
                .upserts
                .contains_key(&(table.to_owned(), key.to_owned()))
    }

    fn is_logically_absent(&self, table: &str, key: &str) -> bool {
        self.cleared.contains_key(table)
            || self
                .deletes
                .contains_key(&(table.to_owned(), key.to_owned()))
    }

    /// Snapshot of everything dirty right now. Bumps the generation so
    /// later marks are distinguishable from the ones being flushed.
    fn snapshot(&mut self) -> DirtySnapshot {
        let generation = self.generation;
        self.generation += 1;
        DirtySnapshot {
            generation,
            cleared: self.cleared.keys().cloned().collect(),
            upserts: self.upserts.keys().cloned().collect(),
            deletes: self.deletes.keys().cloned().collect(),
        }
    }

    /// Forgets the marks covered by `snapshot`, keeping anything newer.
    #[cfg(test)]
    fn forget(&mut self, snapshot: &DirtySnapshot) {
        self.forget_cleared(snapshot.cleared.iter(), snapshot.generation);
        self.forget_upserts(snapshot.upserts.iter(), snapshot.generation);
        self.forget_deletes(snapshot.deletes.iter(), snapshot.generation);
    }

    /// Forgets the cleared-table marks that were flushed at `generation`.
    fn forget_cleared<'a>(&mut self, tables: impl Iterator<Item = &'a String>, generation: u64) {
        for table in tables {
            if self
                .cleared
                .get(table)
                .is_some_and(|marked| *marked <= generation)
            {
                self.cleared.remove(table);
            }
        }
    }

    /// Forgets the upsert marks that were flushed at `generation`.
    fn forget_upserts<'a>(
        &mut self,
        keys: impl Iterator<Item = &'a (String, String)>,
        generation: u64,
    ) {
        for key in keys {
            if self
                .upserts
                .get(key)
                .is_some_and(|marked| *marked <= generation)
            {
                self.upserts.remove(key);
            }
        }
    }

    /// Forgets the delete marks that were flushed at `generation`.
    fn forget_deletes<'a>(
        &mut self,
        keys: impl Iterator<Item = &'a (String, String)>,
        generation: u64,
    ) {
        for key in keys {
            if self
                .deletes
                .get(key)
                .is_some_and(|marked| *marked <= generation)
            {
                self.deletes.remove(key);
            }
        }
    }
}

#[derive(Debug)]
struct DirtySnapshot {
    generation: u64,
    cleared: HashSet<String>,
    upserts: HashSet<(String, String)>,
    deletes: HashSet<(String, String)>,
}

impl DirtySnapshot {
    fn is_empty(&self) -> bool {
        self.cleared.is_empty() && self.upserts.is_empty() && self.deletes.is_empty()
    }
}

#[derive(Clone)]
pub struct Repository {
    pool: PgPool,
    ensured_tables: Arc<RwLock<HashSet<String>>>,
    /// Write-behind store: table -> key -> entry. Writes land here first and
    /// are batch-flushed to `PostgreSQL` every `flush_interval`.
    memory: Arc<RwLock<Store>>,
    dirty: Arc<Mutex<DirtyTracker>>,
    flush_interval: Duration,
    flush_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    write_locks: Arc<[Mutex<()>; MUTEX_SHARDS]>,
    /// Keys currently being loaded from `PostgreSQL`; concurrent misses on
    /// the same key share one SELECT + parse instead of each doing their own.
    loading: Arc<Mutex<LoadLocks>>,
    memory_hits: Arc<AtomicU64>,
    memory_misses: Arc<AtomicU64>,
    /// Idle documents are packed, then (once clean) dropped from memory and
    /// reloaded from `PostgreSQL` on their next access.
    policy: MemoryPolicy,
    memory_entries: Arc<AtomicU64>,
    memory_evicted: Arc<AtomicU64>,
    memory_packed: Arc<AtomicU64>,
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

        let repository = Self::with_pool(
            pool,
            Duration::from_secs(config.flush_interval_secs),
            MemoryPolicy::new(config.pack_idle_secs, config.idle_eviction_secs),
        );
        let flush_interval = repository.flush_interval;
        let task = tokio::spawn(repository.clone().flush_loop(flush_interval));
        *repository.flush_task.lock().await = Some(task);
        Ok(repository)
    }

    fn with_pool(pool: PgPool, flush_interval: Duration, policy: MemoryPolicy) -> Self {
        Self {
            pool,
            ensured_tables: Arc::new(RwLock::new(HashSet::new())),
            memory: Arc::new(RwLock::new(HashMap::new())),
            dirty: Arc::new(Mutex::new(DirtyTracker::default())),
            flush_interval,
            flush_task: Arc::new(Mutex::new(None)),
            write_locks: Arc::new(std::array::from_fn(|_| Mutex::new(()))),
            loading: Arc::new(Mutex::new(HashMap::new())),
            memory_hits: Arc::new(AtomicU64::new(0)),
            memory_misses: Arc::new(AtomicU64::new(0)),
            policy,
            memory_entries: Arc::new(AtomicU64::new(0)),
            memory_evicted: Arc::new(AtomicU64::new(0)),
            memory_packed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Runs `read` against the stored document and returns its result. Only
    /// what the closure copies out leaves the store: a nested `get` on a
    /// multi-megabyte document costs a few hundred nanoseconds, not a deep
    /// clone of the whole document.
    pub async fn read<R>(
        &self,
        table: &str,
        key: &str,
        read: impl FnOnce(&Value) -> R,
    ) -> Result<Option<R>, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.read");
        let Some(entry) = self.load(table, key).await? else {
            return Ok(None);
        };
        let mut guard = entry.read().await;
        if guard.is_expired() {
            return Ok(None);
        }
        if guard.packed.is_some() {
            drop(guard);
            let mut inner = entry.write().await;
            inner.unpack().await?;
            guard = inner.downgrade();
            if guard.is_expired() {
                return Ok(None);
            }
        }
        Ok(Some(read(&guard.value)))
    }

    pub async fn set(&self, table: &str, key: &str, value: Value) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.set");
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        self.store(table, key, value, None).await;
        self.mark_upsert(table, key).await;
        Ok(())
    }

    pub async fn set_with_ttl(
        &self,
        table: &str,
        key: &str,
        value: Value,
        ttl: Duration,
    ) -> Result<(), AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, ttl = ?ttl, "repository.set_with_ttl");
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        self.store(table, key, value, Some(Instant::now() + ttl))
            .await;
        self.mark_upsert(table, key).await;
        Ok(())
    }

    /// Deletes a whole entry; returns whether it existed (in memory or in
    /// `PostgreSQL`) before the call.
    pub async fn delete(&self, table: &str, key: &str) -> Result<bool, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.delete");
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        let existed = match self.remove_memory(table, key).await {
            Some(entry) => !entry.read().await.is_expired(),
            None => self.exists_in_postgres(table, key).await?,
        };
        self.mark_delete(table, key).await;
        Ok(existed)
    }

    /// Deletes the entry only if it carries an elapsed TTL. Used by the
    /// timer spawned for `cache` so that a value stored permanently after
    /// the `cache` call is not wiped when the old TTL fires.
    pub async fn delete_if_expired(&self, table: &str, key: &str) -> Result<bool, AppError> {
        self.ensure_table(table).await?;
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        let Some(entry) = self.lookup(table, key).await else {
            return Ok(false);
        };
        if !entry.read().await.is_expired() {
            return Ok(false);
        }
        self.remove_if_same(table, key, &entry).await;
        self.mark_delete(table, key).await;
        Ok(true)
    }

    pub async fn list_all(&self, table: &str) -> Result<Vec<(String, Value)>, AppError> {
        self.ensure_table(table).await?;
        tracing::trace!(table, "repository.list_all");

        let mut merged: HashMap<String, Value> = HashMap::new();

        // Do not hold any store lock during the (potentially slow) table
        // scan: fetch the "cleared" flag, run the query, then merge memory.
        let cleared = self.dirty.lock().await.cleared.contains_key(table);

        if !cleared {
            let select = format!(r#"SELECT id, value FROM "{table}" ORDER BY id LIMIT $1"#);
            let rows = sqlx::query(&select)
                .bind(MAX_LIST_ROWS)
                .fetch_all(&self.pool)
                .await?;
            let mut raw_rows: Vec<(String, String)> = Vec::with_capacity(rows.len());
            for row in rows {
                raw_rows.push((row.try_get("id")?, row.try_get("value")?));
            }
            for (id, value) in parse_rows(raw_rows).await? {
                merged.insert(id, value);
            }
        }

        let (upserts, deletes) = {
            let dirty = self.dirty.lock().await;
            let upserts: Vec<String> = dirty
                .upserts
                .keys()
                .filter(|(t, _)| t == table)
                .map(|(_, key)| key.clone())
                .collect();
            let deletes: Vec<String> = dirty
                .deletes
                .keys()
                .filter(|(t, _)| t == table)
                .map(|(_, key)| key.clone())
                .collect();
            (upserts, deletes)
        };
        for key in &deletes {
            merged.remove(key);
        }

        // Grab the entries under the store lock, read them outside of it.
        let entries: Vec<(String, Entry)> = {
            let tables = self.memory.read().await;
            match tables.get(table) {
                Some(rows) => upserts
                    .into_iter()
                    .filter_map(|key| rows.get(&key).map(|entry| (key, Arc::clone(entry))))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (index, (key, entry)) in entries.into_iter().enumerate() {
            {
                let guard = entry.read().await;
                if !guard.is_expired() {
                    merged.insert(key, guard.to_value().await?);
                }
            }
            if index % YIELD_EVERY == YIELD_EVERY - 1 {
                tokio::task::yield_now().await;
            }
        }

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

    /// Read-modify-write of one document, in place. `transform` receives
    /// the stored document (or `absent` when there is none) and returns
    /// `Some(result)` to keep the document, `None` to delete it. The per-key
    /// lock makes the whole operation atomic against every other write on
    /// the same key.
    ///
    /// On error the document is left untouched: every mutation in
    /// `values` validates its target before it modifies anything.
    pub async fn mutate<R>(
        &self,
        table: &str,
        key: &str,
        absent: Value,
        expiry: Expiry,
        transform: impl FnOnce(&mut Value) -> Result<Option<R>, AppError>,
    ) -> Result<Option<R>, AppError> {
        assert_key_fits(key)?;
        self.ensure_table(table).await?;
        tracing::trace!(table, key, "repository.mutate");

        // Warm the document before taking the shard lock: a cold load (pool
        // wait + transfer + parse) must not stall the other keys hashed to
        // the same shard. The lookup under the lock below is then a hit.
        self.load(table, key).await?;
        let _guard = self.write_locks[mutate_shard(table, key)].lock().await;
        // A missing document is edited on a detached entry that only becomes
        // visible once the transform succeeded, so a failed `add`/`push` on
        // an unknown key never leaves an empty placeholder behind.
        let (entry, detached) = match self.load(table, key).await? {
            Some(entry) => (entry, false),
            None => (new_entry(absent.clone(), None), true),
        };
        let outcome = {
            let mut inner = entry.write().await;
            if inner.is_expired() {
                inner.replace(absent);
                inner.expires_at = None;
            } else {
                inner.unpack().await?;
            }
            let outcome = transform(&mut inner.value)?;
            if outcome.is_some() {
                inner.expires_at = match expiry {
                    Expiry::Clear => None,
                    Expiry::Set(at) => Some(at),
                };
            }
            outcome
        };
        if let Some(result) = outcome {
            if detached || !self.is_attached(table, key, &entry).await {
                self.attach(table, key, entry).await;
            }
            self.mark_upsert(table, key).await;
            Ok(Some(result))
        } else {
            if !detached {
                self.remove_if_same(table, key, &entry).await;
            }
            self.mark_delete(table, key).await;
            Ok(None)
        }
    }

    /// Removes expired TTL entries; returns how many were purged.
    #[cfg(test)]
    pub async fn purge_expired(&self) -> usize {
        self.sweep_at(now_secs(), MemoryPolicy::default())
            .await
            .purged
    }

    /// Periodic maintenance of the in-memory store: purges expired TTL
    /// entries, evicts the clean ones idle for `policy.evict_after` and
    /// packs the ones idle for `policy.pack_after`.
    pub async fn sweep(&self) -> SweepOutcome {
        self.sweep_at(now_secs(), self.policy).await
    }

    async fn sweep_at(&self, now: u64, policy: MemoryPolicy) -> SweepOutcome {
        let idle_eviction = policy.evict_after;
        // Only the candidates are copied out: cloning every key each minute
        // would churn the allocator for entries that are kept anyway. An
        // entry whose lock is busy is in use, so it is neither expired nor
        // idle; the next sweep looks at it again.
        let mut already_packed = 0usize;
        let mut to_pack: Vec<Entry> = Vec::new();
        let candidates: Vec<(String, String, Entry, SweepReason)> = {
            let tables = self.memory.read().await;
            let mut candidates = Vec::new();
            for (table, entries) in tables.iter() {
                for (key, entry) in entries {
                    let Ok(guard) = entry.try_read() else {
                        continue;
                    };
                    let idle = entry.idle_for(now);
                    let reason = if guard.is_expired() {
                        SweepReason::Expired
                    } else if idle_eviction.is_some_and(|limit| idle >= limit) {
                        SweepReason::Idle
                    } else {
                        if guard.packed.is_some() {
                            already_packed += 1;
                        } else if guard.is_packable()
                            && policy.pack_after.is_some_and(|limit| idle >= limit)
                        {
                            to_pack.push(Arc::clone(entry));
                        }
                        continue;
                    };
                    candidates.push((table.clone(), key.clone(), Arc::clone(entry), reason));
                }
            }
            candidates
        };

        let mut outcome = SweepOutcome::default();
        for (index, (table, key, entry, reason)) in candidates.into_iter().enumerate() {
            // Same lock as every writer: no write can land between the checks
            // below and the removal, so a dirty or revived entry is never
            // dropped.
            let _guard = self.write_locks[mutate_shard(&table, &key)].lock().await;
            let (expired, has_ttl) = {
                let inner = entry.read().await;
                (inner.is_expired(), inner.expires_at.is_some())
            };
            if reason == SweepReason::Expired {
                if expired && self.remove_if_same(&table, &key, &entry).await {
                    self.mark_delete(&table, &key).await;
                    outcome.purged += 1;
                }
            } else if self
                .evictable(&table, &key, &entry, has_ttl, now, idle_eviction)
                .await
            {
                if self.remove_if_same(&table, &key, &entry).await {
                    outcome.evicted += 1;
                }
            } else if policy.pack_after.is_some() {
                // Still pending a flush (or carrying a TTL): it cannot leave
                // memory yet, but it can shrink.
                to_pack.push(entry);
            }
            if index % YIELD_EVERY == YIELD_EVERY - 1 {
                tokio::task::yield_now().await;
            }
        }

        outcome.packed = pack_idle(to_pack, now, policy.pack_after).await;

        let entries = if outcome.purged + outcome.evicted > 0 {
            let mut tables = self.memory.write().await;
            tables.retain(|_, entries| !entries.is_empty());
            for entries in tables.values_mut() {
                if entries.len() * SHRINK_RATIO < entries.capacity() {
                    entries.shrink_to_fit();
                }
            }
            if tables.len() * SHRINK_RATIO < tables.capacity() {
                tables.shrink_to_fit();
            }
            tables.values().map(HashMap::len).sum::<usize>()
        } else {
            let tables = self.memory.read().await;
            tables.values().map(HashMap::len).sum::<usize>()
        };
        self.memory_entries.store(
            u64::try_from(entries).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.memory_packed.store(
            u64::try_from(already_packed + outcome.packed).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.memory_evicted.fetch_add(
            u64::try_from(outcome.evicted).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        outcome
    }

    /// Whether an idle candidate may leave memory. Caller holds the key's
    /// write lock. TTL entries are left to the purge: their deadline is not
    /// stored in `PostgreSQL`, so a reload would make them permanent. Dirty
    /// entries stay until a flush made `PostgreSQL` current.
    async fn evictable(
        &self,
        table: &str,
        key: &str,
        entry: &Entry,
        has_ttl: bool,
        now: u64,
        idle_eviction: Option<u64>,
    ) -> bool {
        !has_ttl
            && idle_eviction.is_some_and(|limit| entry.idle_for(now) >= limit)
            && !self.dirty.lock().await.is_pending(table, key)
    }

    /// Batch-pushes all buffered writes to `PostgreSQL`. Safe to call
    /// concurrently with writes: entries written during the flush stay dirty
    /// for the next cycle. Every statement that succeeded is forgotten right
    /// away, so a failing statement only leaves its own rows (and everything
    /// after it) for the next cycle instead of retrying the whole batch.
    pub async fn flush(&self) -> Result<(), AppError> {
        let snapshot = self.dirty.lock().await.snapshot();
        if snapshot.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let generation = snapshot.generation;

        // 1) Fully cleared tables first.
        for table in &snapshot.cleared {
            let delete = format!(r#"DELETE FROM "{table}""#);
            sqlx::query(&delete).execute(&self.pool).await?;
            self.dirty
                .lock()
                .await
                .forget_cleared(std::iter::once(table), generation);
        }

        // 2) Key deletions, batched per table.
        let mut deletes_by_table: HashMap<String, Vec<String>> = HashMap::new();
        for (table, key) in &snapshot.deletes {
            if snapshot.cleared.contains(table) {
                continue;
            }
            deletes_by_table
                .entry(table.clone())
                .or_default()
                .push(key.clone());
        }
        for (table, keys) in &deletes_by_table {
            let delete = format!(r#"DELETE FROM "{table}" WHERE id = ANY($1::text[])"#);
            for chunk in keys.chunks(FLUSH_DELETES_PER_STATEMENT) {
                sqlx::query(&delete).bind(chunk).execute(&self.pool).await?;
                let flushed: Vec<(String, String)> = chunk
                    .iter()
                    .map(|key| (table.clone(), key.clone()))
                    .collect();
                self.dirty
                    .lock()
                    .await
                    .forget_deletes(flushed.iter(), generation);
            }
        }
        // Deletes of tables that were cleared are covered by the wipe above.
        {
            let covered: Vec<&(String, String)> = snapshot
                .deletes
                .iter()
                .filter(|(table, _)| snapshot.cleared.contains(table))
                .collect();
            self.dirty
                .lock()
                .await
                .forget_deletes(covered.into_iter(), generation);
        }

        // 3) Upserts: serialize the freshest in-memory value of each dirty key.
        let mut upserts_by_table: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (index, (table, key)) in snapshot.upserts.iter().enumerate() {
            let payload = match self.lookup(table, key).await {
                Some(entry) => {
                    let guard = entry.read().await;
                    if guard.is_expired() {
                        None
                    } else {
                        Some(guard.to_json()?)
                    }
                }
                None => None,
            };
            let Some(payload) = payload else {
                // Deleted, expired, or purged before the flush landed; make
                // sure the stale row (if any) is removed from `PostgreSQL`.
                self.mark_delete(table, key).await;
                continue;
            };
            upserts_by_table
                .entry(table.clone())
                .or_default()
                .push((key.clone(), payload));
            if index % YIELD_EVERY == YIELD_EVERY - 1 {
                tokio::task::yield_now().await;
            }
        }
        let mut statements = 0usize;
        for (table, rows) in &upserts_by_table {
            self.ensure_table(table).await?;
            let insert = format!(
                r#"INSERT INTO "{table}" (id, value) SELECT * FROM UNNEST($1::text[], $2::text[]) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value"#
            );
            for chunk in chunk_rows(rows) {
                let ids: Vec<&str> = chunk.iter().map(|(id, _)| id.as_str()).collect();
                let payloads: Vec<&str> =
                    chunk.iter().map(|(_, payload)| payload.as_str()).collect();
                sqlx::query(&insert)
                    .bind(&ids)
                    .bind(&payloads)
                    .execute(&self.pool)
                    .await?;
                statements += 1;
                // Only forget what was actually flushed; writes that landed
                // meanwhile carry a newer generation and stay dirty.
                let flushed: Vec<(String, String)> = chunk
                    .iter()
                    .map(|(id, _)| (table.clone(), id.clone()))
                    .collect();
                self.dirty
                    .lock()
                    .await
                    .forget_upserts(flushed.iter(), generation);
            }
        }

        tracing::info!(
            cleared = snapshot.cleared.len(),
            deletes = snapshot.deletes.len(),
            upserts = snapshot.upserts.len(),
            statements,
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

    pub fn memory_stats(&self) -> MemoryStats {
        MemoryStats {
            hits: self.memory_hits.load(Ordering::Relaxed),
            misses: self.memory_misses.load(Ordering::Relaxed),
            entries: self.memory_entries.load(Ordering::Relaxed),
            evicted: self.memory_evicted.load(Ordering::Relaxed),
            packed: self.memory_packed.load(Ordering::Relaxed),
        }
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

    /// Finds the entry for a key, loading it from `PostgreSQL` into memory
    /// when it was never touched since startup. Returns expired entries as
    /// well: readers treat them as absent, writers reset them.
    async fn load(&self, table: &str, key: &str) -> Result<Option<Entry>, AppError> {
        if let Some(entry) = self.lookup(table, key).await {
            self.memory_hits.fetch_add(1, Ordering::Relaxed);
            entry.touch();
            return Ok(Some(entry));
        }
        self.memory_misses.fetch_add(1, Ordering::Relaxed);

        // The key may be logically absent: deleted or expired since the last
        // flush, or its table was cleared. `PostgreSQL` may still hold the
        // old row until the next flush deletes it, so it must not be
        // resurrected here.
        if self.dirty.lock().await.is_logically_absent(table, key) {
            return Ok(None);
        }

        // Single-flight: whoever holds the per-key load lock does the round
        // trip, everyone else waits for it and then finds the entry in memory.
        let tuple = (table.to_owned(), key.to_owned());
        let load_lock = Arc::clone(
            self.loading
                .lock()
                .await
                .entry(tuple.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        );
        let result = {
            let _loading = load_lock.lock().await;
            match self.lookup(table, key).await {
                Some(entry) => Ok(Some(entry)),
                None => self.load_from_postgres(table, key).await,
            }
        };
        {
            let mut loading = self.loading.lock().await;
            if loading
                .get(&tuple)
                .is_some_and(|current| Arc::ptr_eq(current, &load_lock))
                && Arc::strong_count(&load_lock) == 2
            {
                loading.remove(&tuple);
            }
        }
        result
    }

    async fn load_from_postgres(&self, table: &str, key: &str) -> Result<Option<Entry>, AppError> {
        let select = format!(r#"SELECT value FROM "{table}" WHERE id = $1"#);
        let row = sqlx::query(&select)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        let Some(raw) = row
            .as_ref()
            .map(|row| row.try_get::<String, _>("value"))
            .transpose()?
        else {
            return Ok(None);
        };
        let value = parse_json(raw).await?;

        // The key may have been written to memory while we were reading
        // PostgreSQL; the entry that is already there wins.
        let mut tables = self.memory.write().await;
        let entries = tables.entry(table.to_owned()).or_default();
        let entry = entries
            .entry(key.to_owned())
            .or_insert_with(|| new_entry(value, None));
        Ok(Some(Arc::clone(entry)))
    }

    async fn lookup(&self, table: &str, key: &str) -> Option<Entry> {
        let tables = self.memory.read().await;
        tables
            .get(table)
            .and_then(|entries| entries.get(key))
            .map(Arc::clone)
    }

    async fn is_attached(&self, table: &str, key: &str, entry: &Entry) -> bool {
        let tables = self.memory.read().await;
        tables
            .get(table)
            .and_then(|entries| entries.get(key))
            .is_some_and(|current| Arc::ptr_eq(current, entry))
    }

    async fn attach(&self, table: &str, key: &str, entry: Entry) {
        let mut tables = self.memory.write().await;
        tables
            .entry(table.to_owned())
            .or_default()
            .insert(key.to_owned(), entry);
    }

    /// Replaces the value of a key (creating the entry when needed) without
    /// swapping the entry out from under concurrent readers.
    async fn store(&self, table: &str, key: &str, value: Value, expires_at: Option<Instant>) {
        let mut pending = Some(value);
        let existing = {
            let mut tables = self.memory.write().await;
            let entries = tables.entry(table.to_owned()).or_default();
            match entries.entry(key.to_owned()) {
                MapSlot::Occupied(slot) => Some(Arc::clone(slot.get())),
                MapSlot::Vacant(slot) => {
                    if let Some(value) = pending.take() {
                        slot.insert(new_entry(value, expires_at));
                    }
                    None
                }
            }
        };
        if let (Some(entry), Some(value)) = (existing, pending) {
            entry.touch();
            let mut inner = entry.write().await;
            inner.replace(value);
            inner.expires_at = expires_at;
        }
    }

    async fn remove_memory(&self, table: &str, key: &str) -> Option<Entry> {
        let mut tables = self.memory.write().await;
        tables
            .get_mut(table)
            .and_then(|entries| entries.remove(key))
    }

    /// Removes the key only if it still maps to `entry`; returns whether it
    /// was removed.
    async fn remove_if_same(&self, table: &str, key: &str, entry: &Entry) -> bool {
        let mut tables = self.memory.write().await;
        let Some(entries) = tables.get_mut(table) else {
            return false;
        };
        if entries
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            entries.remove(key);
            true
        } else {
            false
        }
    }

    async fn exists_in_postgres(&self, table: &str, key: &str) -> Result<bool, AppError> {
        if self.dirty.lock().await.is_logically_absent(table, key) {
            return Ok(false);
        }
        let select = format!(r#"SELECT 1 FROM "{table}" WHERE id = $1"#);
        Ok(sqlx::query(&select)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?
            .is_some())
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
        // The DDL is idempotent, so it runs without holding the write lock:
        // every other request would otherwise queue behind this round trip
        // (tokio's RwLock is write-preferring) each time a table is first seen.
        let create = format!(
            r#"CREATE TABLE IF NOT EXISTS "{table}" (id VARCHAR(255) PRIMARY KEY, value TEXT)"#
        );
        match sqlx::query(&create).execute(&self.pool).await {
            Ok(_) => {}
            Err(error) => {
                // Concurrent `CREATE TABLE IF NOT EXISTS` on a missing table:
                // the loser gets either `duplicate_table` or a unique
                // violation on the catalog's type-name index. PostgreSQL puts
                // the index name in the constraint field, not the message.
                let tolerated = error.as_database_error().is_some_and(|database_error| {
                    database_error
                        .code()
                        .is_some_and(|code| code == DUPLICATE_TABLE_SQLSTATE)
                        || database_error.constraint() == Some(TYPE_NAME_INDEX)
                        || database_error.message().contains(TYPE_NAME_INDEX)
                });
                if !tolerated {
                    return Err(AppError::Database(error));
                }
            }
        }
        self.ensured_tables.write().await.insert(table.to_owned());
        Ok(())
    }
}

/// Packs the idle entries on the blocking pool (serializing a large tree
/// would stall an async worker). An entry that is locked, or was used since
/// the scan, is skipped: a document in use stays hot. Returns how many were
/// packed.
async fn pack_idle(entries: Vec<Entry>, now: u64, pack_after: Option<u64>) -> usize {
    let Some(limit) = pack_after else {
        return 0;
    };
    if entries.is_empty() {
        return 0;
    }
    tokio::task::spawn_blocking(move || {
        entries
            .iter()
            .filter(|entry| entry.idle_for(now) >= limit)
            .filter(|entry| entry.try_write().is_ok_and(|mut inner| inner.pack()))
            .count()
    })
    .await
    .unwrap_or(0)
}

/// Parses a packed document, moving large ones off the async workers.
async fn parse_packed(text: Arc<str>) -> Result<Value, AppError> {
    if text.len() < BLOCKING_PARSE_THRESHOLD {
        return Ok(serde_json::from_str(&text)?);
    }
    tokio::task::spawn_blocking(move || serde_json::from_str::<Value>(&text))
        .await
        .map_err(|error| AppError::internal(format!("json parse task failed: {error}")))?
        .map_err(AppError::from)
}

/// Parses one JSON payload, moving large ones off the async workers.
async fn parse_json(raw: String) -> Result<Value, AppError> {
    if raw.len() < BLOCKING_PARSE_THRESHOLD {
        return Ok(serde_json::from_str(&raw)?);
    }
    tokio::task::spawn_blocking(move || serde_json::from_str::<Value>(&raw))
        .await
        .map_err(|error| AppError::internal(format!("json parse task failed: {error}")))?
        .map_err(AppError::from)
}

/// Parses a batch of rows on the blocking pool (table scans can be large).
async fn parse_rows(rows: Vec<(String, String)>) -> Result<Vec<(String, Value)>, AppError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|(id, raw)| serde_json::from_str::<Value>(&raw).map(|value| (id, value)))
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|error| AppError::internal(format!("json parse task failed: {error}")))?
    .map_err(AppError::from)
}

/// Splits flush rows into statements bounded by row count and payload size.
fn chunk_rows(rows: &[(String, String)]) -> Vec<&[(String, String)]> {
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;
    for (index, (_, payload)) in rows.iter().enumerate() {
        let count = index - start;
        if count > 0
            && (count >= FLUSH_ROWS_PER_STATEMENT
                || bytes + payload.len() > FLUSH_BYTES_PER_STATEMENT)
        {
            chunks.push(&rows[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += payload.len();
    }
    if start < rows.len() {
        chunks.push(&rows[start..]);
    }
    chunks
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A repository whose pool never connects: only the in-memory paths are
    /// exercised. The table is pre-marked as ensured and as cleared, so
    /// unknown keys count as logically absent instead of being looked up.
    fn memory_only() -> Repository {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
            .expect("lazy pool");
        let repository =
            Repository::with_pool(pool, Duration::from_mins(5), MemoryPolicy::default());
        repository
            .ensured_tables
            .try_write()
            .expect("unlocked")
            .insert("t".to_owned());
        repository
            .dirty
            .try_lock()
            .expect("unlocked")
            .clear_table("t");
        repository
    }

    #[test]
    fn tracker_tracks_upserts_deletes_and_table_clears() {
        let mut tracker = DirtyTracker::default();

        tracker.mark_upsert("t", "a");
        tracker.mark_upsert("t", "b");
        assert!(
            tracker
                .upserts
                .contains_key(&("t".to_owned(), "a".to_owned()))
        );

        tracker.mark_delete("t", "a");
        assert!(
            !tracker
                .upserts
                .contains_key(&("t".to_owned(), "a".to_owned()))
        );
        assert!(
            tracker
                .deletes
                .contains_key(&("t".to_owned(), "a".to_owned()))
        );
        assert!(
            tracker
                .upserts
                .contains_key(&("t".to_owned(), "b".to_owned()))
        );

        tracker.mark_upsert("t", "a");
        assert!(
            !tracker
                .deletes
                .contains_key(&("t".to_owned(), "a".to_owned()))
        );
        assert!(
            tracker
                .upserts
                .contains_key(&("t".to_owned(), "a".to_owned()))
        );

        tracker.clear_table("t");
        assert!(tracker.cleared.contains_key("t"));
        assert!(tracker.upserts.is_empty());
        assert!(tracker.deletes.is_empty());
    }

    #[test]
    fn tracker_keeps_marks_made_during_a_flush() {
        let mut tracker = DirtyTracker::default();
        tracker.mark_upsert("t", "a");
        tracker.mark_delete("t", "b");

        let snapshot = tracker.snapshot();
        // Writes that land while the snapshot is being flushed.
        tracker.mark_upsert("t", "a");
        tracker.mark_upsert("t", "c");

        tracker.forget(&snapshot);
        assert!(
            tracker
                .upserts
                .contains_key(&("t".to_owned(), "a".to_owned()))
        );
        assert!(
            tracker
                .upserts
                .contains_key(&("t".to_owned(), "c".to_owned()))
        );
        assert!(
            !tracker
                .deletes
                .contains_key(&("t".to_owned(), "b".to_owned()))
        );

        let second = tracker.snapshot();
        tracker.forget(&second);
        assert!(tracker.upserts.is_empty());
    }

    #[test]
    fn mem_entry_expires_only_with_elapsed_ttl() {
        let fresh = MemEntry {
            packed: None,
            value: json!(1),
            expires_at: Some(Instant::now() + Duration::from_mins(1)),
        };
        assert!(!fresh.is_expired());

        let expired = MemEntry {
            packed: None,
            value: json!(1),
            expires_at: Some(Instant::now()),
        };
        assert!(expired.is_expired());

        let never = MemEntry {
            packed: None,
            value: json!(1),
            expires_at: None,
        };
        assert!(!never.is_expired());
    }

    #[test]
    fn chunk_rows_bounds_rows_and_bytes() {
        let rows: Vec<(String, String)> = (0..1_250)
            .map(|index| (index.to_string(), "x".repeat(10)))
            .collect();
        let chunks = chunk_rows(&rows);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), FLUSH_ROWS_PER_STATEMENT);
        assert_eq!(chunks[2].len(), 250);

        let big: Vec<(String, String)> = (0..3)
            .map(|index| (index.to_string(), "x".repeat(FLUSH_BYTES_PER_STATEMENT)))
            .collect();
        assert_eq!(chunk_rows(&big).len(), 3);
        assert!(chunk_rows(&[]).is_empty());
    }

    #[tokio::test]
    async fn read_copies_only_the_requested_part() {
        let repository = memory_only();
        repository
            .set(
                "t",
                "guild",
                json!({"GUILD": {"LANG": {"lang": "fr"}}, "STATS": {}}),
            )
            .await
            .unwrap();

        let lang = repository
            .read("t", "guild", |doc| doc["GUILD"]["LANG"]["lang"].clone())
            .await
            .unwrap();
        assert_eq!(lang, Some(json!("fr")));
        assert_eq!(
            repository.read("t", "missing", Value::clone).await.unwrap(),
            None
        );
        // Reads of unknown keys never leave placeholders behind.
        assert!(repository.lookup("t", "missing").await.is_none());
    }

    #[tokio::test]
    async fn mutate_edits_in_place_and_deletes_when_asked() {
        let repository = memory_only();
        let pushed = repository
            .mutate("t", "doc", json!([]), Expiry::Clear, |doc| {
                crate::values::push_element(doc, &[], json!(1))?;
                Ok(Some(doc.clone()))
            })
            .await
            .unwrap();
        assert_eq!(pushed, Some(json!([1])));
        assert_eq!(
            repository.read("t", "doc", Value::clone).await.unwrap(),
            Some(json!([1]))
        );

        let gone: Option<()> = repository
            .mutate("t", "doc", json!([]), Expiry::Clear, |_| Ok(None))
            .await
            .unwrap();
        assert!(gone.is_none());
        assert!(repository.lookup("t", "doc").await.is_none());
        assert!(
            repository
                .dirty
                .lock()
                .await
                .deletes
                .contains_key(&("t".to_owned(), "doc".to_owned()))
        );
    }

    #[tokio::test]
    async fn failed_mutation_on_unknown_key_leaves_nothing_behind() {
        let repository = memory_only();
        let result: Result<Option<()>, AppError> = repository
            .mutate("t", "doc", json!({}), Expiry::Clear, |_| {
                Err(AppError::validation("boom"))
            })
            .await;
        assert!(result.is_err());
        assert!(repository.lookup("t", "doc").await.is_none());
        assert!(repository.dirty.lock().await.upserts.is_empty());
    }

    #[tokio::test]
    async fn concurrent_mutations_on_one_key_never_lose_updates() {
        let repository = memory_only();
        let mut tasks = Vec::new();
        for _ in 0..200 {
            let repository = repository.clone();
            tasks.push(tokio::spawn(async move {
                repository
                    .mutate("t", "counter", json!({}), Expiry::Clear, |doc| {
                        crate::values::add_amount(doc, &["n".to_owned()], 1.0)?;
                        Ok(Some(()))
                    })
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let total = repository
            .read("t", "counter", |doc| doc["n"].as_f64())
            .await
            .unwrap()
            .flatten();
        assert_eq!(total, Some(200.0));
    }

    #[tokio::test]
    async fn expired_entries_read_as_absent_and_are_purged() {
        let repository = memory_only();
        repository
            .set_with_ttl("t", "tmp", json!({"a": 1}), Duration::from_millis(10))
            .await
            .unwrap();
        assert_eq!(
            repository.read("t", "tmp", Value::clone).await.unwrap(),
            Some(json!({"a": 1}))
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            repository.read("t", "tmp", Value::clone).await.unwrap(),
            None
        );
        assert!(!repository.delete_if_expired("t", "unknown").await.unwrap());
        assert_eq!(repository.purge_expired().await, 1);
        assert!(repository.lookup("t", "tmp").await.is_none());
        assert!(
            repository
                .dirty
                .lock()
                .await
                .deletes
                .contains_key(&("t".to_owned(), "tmp".to_owned()))
        );
        // Deleting a key that is only known as deleted does not touch the pool.
        assert!(!repository.delete("t", "tmp").await.unwrap());
    }

    #[tokio::test]
    async fn ttl_timer_does_not_wipe_a_value_stored_permanently_afterwards() {
        let repository = memory_only();
        repository
            .set_with_ttl("t", "k", json!("temporary"), Duration::from_millis(10))
            .await
            .unwrap();
        repository.set("t", "k", json!("permanent")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!repository.delete_if_expired("t", "k").await.unwrap());
        assert_eq!(
            repository.read("t", "k", Value::clone).await.unwrap(),
            Some(json!("permanent"))
        );
    }

    #[tokio::test]
    async fn mutate_resets_expired_documents_to_absent() {
        let repository = memory_only();
        repository
            .set_with_ttl("t", "k", json!({"old": true}), Duration::from_millis(10))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        repository
            .mutate("t", "k", json!({}), Expiry::Clear, |doc| {
                crate::values::set_nested(doc, &["fresh".to_owned()], json!(1))?;
                Ok(Some(()))
            })
            .await
            .unwrap();
        assert_eq!(
            repository.read("t", "k", Value::clone).await.unwrap(),
            Some(json!({"fresh": 1}))
        );
    }

    #[tokio::test]
    async fn delete_reports_existence_from_memory() {
        let repository = memory_only();
        repository.set("t", "k", json!(1)).await.unwrap();
        assert!(repository.delete("t", "k").await.unwrap());
        assert!(!repository.delete("t", "k").await.unwrap());
        assert_eq!(repository.read("t", "k", Value::clone).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_table_hides_every_key_without_touching_the_pool() {
        let repository = memory_only();
        repository.set("t", "k", json!(1)).await.unwrap();
        repository.clear_table("t").await.unwrap();
        assert_eq!(repository.read("t", "k", Value::clone).await.unwrap(), None);
        assert!(!repository.delete("t", "k").await.unwrap());
    }

    /// Forgets every dirty mark, as if a flush had pushed everything to
    /// `PostgreSQL` (the table is no longer considered cleared either).
    async fn pretend_flushed(repository: &Repository) {
        let mut dirty = repository.dirty.lock().await;
        let snapshot = dirty.snapshot();
        dirty.forget(&snapshot);
    }

    fn evict(after: u64) -> MemoryPolicy {
        MemoryPolicy {
            pack_after: None,
            evict_after: Some(after),
        }
    }

    async fn age(repository: &Repository, key: &str, last_access: u64) {
        repository
            .lookup("t", key)
            .await
            .unwrap()
            .last_access
            .store(last_access, Ordering::Relaxed);
    }

    #[tokio::test]
    async fn idle_flushed_entries_are_evicted_and_maps_shrink() {
        let repository = memory_only();
        for index in 0..256 {
            repository
                .set("t", &format!("k{index}"), json!({"i": index}))
                .await
                .unwrap();
        }
        pretend_flushed(&repository).await;

        let outcome = repository.sweep_at(now_secs() + 1_000, evict(900)).await;
        assert_eq!(
            outcome,
            SweepOutcome {
                purged: 0,
                evicted: 256,
                packed: 0,
            }
        );
        assert!(repository.lookup("t", "k0").await.is_none());
        assert!(repository.memory.read().await.is_empty());
        // Evicted keys are not deleted: they are reloaded on next access.
        assert!(!repository.dirty.lock().await.is_logically_absent("t", "k0"));
        let stats = repository.memory_stats();
        assert_eq!((stats.entries, stats.evicted), (0, 256));
    }

    #[tokio::test]
    async fn dirty_entries_are_never_evicted() {
        let repository = memory_only();
        repository.set("t", "flushed", json!(1)).await.unwrap();
        pretend_flushed(&repository).await;
        // Written again after the flush: memory holds the only copy.
        repository.set("t", "flushed", json!(2)).await.unwrap();
        repository.set("t", "never", json!(3)).await.unwrap();

        let outcome = repository.sweep_at(now_secs() + 1_000, evict(900)).await;
        assert_eq!(outcome.evicted, 0);
        assert_eq!(
            repository.read("t", "flushed", Value::clone).await.unwrap(),
            Some(json!(2))
        );
        assert!(repository.lookup("t", "never").await.is_some());
        assert_eq!(repository.memory_stats().entries, 2);
    }

    #[tokio::test]
    async fn recently_used_entries_stay_resident() {
        let repository = memory_only();
        repository.set("t", "cold", json!(1)).await.unwrap();
        repository.set("t", "hot", json!(2)).await.unwrap();
        pretend_flushed(&repository).await;
        age(&repository, "cold", 0).await;
        age(&repository, "hot", 1_000).await;

        let outcome = repository.sweep_at(1_500, evict(900)).await;
        assert_eq!(outcome.evicted, 1);
        assert!(repository.lookup("t", "cold").await.is_none());
        assert!(repository.lookup("t", "hot").await.is_some());
    }

    #[tokio::test]
    async fn reads_and_writes_refresh_the_access_clock() {
        let repository = memory_only();
        repository.set("t", "k", json!(1)).await.unwrap();
        let entry = repository.lookup("t", "k").await.unwrap();

        entry.last_access.store(u64::MAX, Ordering::Relaxed);
        repository.read("t", "k", Value::clone).await.unwrap();
        assert!(entry.last_access.load(Ordering::Relaxed) <= now_secs());

        entry.last_access.store(u64::MAX, Ordering::Relaxed);
        repository.set("t", "k", json!(2)).await.unwrap();
        assert!(entry.last_access.load(Ordering::Relaxed) <= now_secs());
    }

    #[tokio::test]
    async fn ttl_entries_are_purged_not_evicted() {
        let repository = memory_only();
        repository
            .set_with_ttl("t", "session", json!(1), Duration::from_hours(1))
            .await
            .unwrap();
        pretend_flushed(&repository).await;

        let outcome = repository.sweep_at(now_secs() + 10_000, evict(900)).await;
        assert_eq!(outcome, SweepOutcome::default());
        assert!(repository.lookup("t", "session").await.is_some());
    }

    #[tokio::test]
    async fn disabled_eviction_keeps_everything() {
        let repository = memory_only();
        repository.set("t", "k", json!(1)).await.unwrap();
        pretend_flushed(&repository).await;
        assert_eq!(repository.policy, MemoryPolicy::default());

        let outcome = repository.sweep_at(u64::MAX, repository.policy).await;
        assert_eq!(outcome, SweepOutcome::default());
        assert!(repository.lookup("t", "k").await.is_some());
        assert_eq!(repository.memory_stats().entries, 1);
    }

    fn pack(after: u64) -> MemoryPolicy {
        MemoryPolicy {
            pack_after: Some(after),
            evict_after: None,
        }
    }

    async fn is_packed(repository: &Repository, key: &str) -> bool {
        let entry = repository.lookup("t", key).await.unwrap();
        entry.read().await.packed.is_some()
    }

    fn guild() -> Value {
        json!({"prefix": "+", "members": [{"id": "1", "xp": 10}, {"id": "2", "xp": 20}], "xp": {"on": true}})
    }

    #[tokio::test]
    async fn idle_documents_are_packed_and_read_back_identical() {
        let repository = memory_only();
        repository.set("t", "g", guild()).await.unwrap();
        repository.set("t", "n", json!(42)).await.unwrap();
        age(&repository, "g", 0).await;
        age(&repository, "n", 0).await;

        let outcome = repository.sweep_at(500, pack(120)).await;
        assert_eq!(outcome.packed, 1, "scalars are not worth packing");
        assert!(is_packed(&repository, "g").await);
        assert!(!is_packed(&repository, "n").await);
        assert_eq!(repository.memory_stats().packed, 1);

        // A nested read parses the document back in place.
        assert_eq!(
            repository
                .read("t", "g", |value| value["members"][1]["xp"].clone())
                .await
                .unwrap(),
            Some(json!(20))
        );
        assert!(!is_packed(&repository, "g").await);
        assert_eq!(
            repository.read("t", "g", Value::clone).await.unwrap(),
            Some(guild())
        );
    }

    #[tokio::test]
    async fn recently_used_documents_stay_parsed() {
        let repository = memory_only();
        repository.set("t", "g", guild()).await.unwrap();
        age(&repository, "g", 450).await;
        let outcome = repository.sweep_at(500, pack(120)).await;
        assert_eq!(outcome.packed, 0);
        assert!(!is_packed(&repository, "g").await);
    }

    #[tokio::test]
    async fn mutations_apply_to_packed_documents() {
        let repository = memory_only();
        repository.set("t", "g", guild()).await.unwrap();
        age(&repository, "g", 0).await;
        repository.sweep_at(500, pack(120)).await;
        assert!(is_packed(&repository, "g").await);

        repository
            .mutate("t", "g", Value::Null, Expiry::Clear, |value| {
                value["prefix"] = json!("!");
                Ok(Some(()))
            })
            .await
            .unwrap();
        let mut expected = guild();
        expected["prefix"] = json!("!");
        assert_eq!(
            repository.read("t", "g", Value::clone).await.unwrap(),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn packed_documents_flush_and_list_unchanged() {
        let repository = memory_only();
        repository.set("t", "a", guild()).await.unwrap();
        repository.set("t", "b", json!([1, 2, 3])).await.unwrap();
        age(&repository, "a", 0).await;
        age(&repository, "b", 0).await;
        // Dirty documents may be packed too: the flush reuses their text.
        assert_eq!(repository.sweep_at(500, pack(120)).await.packed, 2);

        let entry = repository.lookup("t", "a").await.unwrap();
        let text = entry.read().await.to_json().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), guild());

        let rows = repository.list_all("t").await.unwrap();
        assert_eq!(
            rows,
            vec![
                ("a".to_owned(), guild()),
                ("b".to_owned(), json!([1, 2, 3]))
            ]
        );
        assert!(is_packed(&repository, "a").await, "listing does not unpack");
    }

    #[tokio::test]
    async fn writes_replace_packed_documents() {
        let repository = memory_only();
        repository.set("t", "g", guild()).await.unwrap();
        age(&repository, "g", 0).await;
        repository.sweep_at(500, pack(120)).await;
        repository
            .set("t", "g", json!({"new": true}))
            .await
            .unwrap();
        assert!(!is_packed(&repository, "g").await);
        assert_eq!(
            repository.read("t", "g", Value::clone).await.unwrap(),
            Some(json!({"new": true}))
        );
    }
}
