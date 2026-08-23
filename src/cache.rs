use std::{collections::HashMap, time::Instant};

use serde_json::Value;
use tokio::sync::RwLock;

#[derive(Debug)]
struct Entry {
    value: Value,
    inserted_at: Instant,
    expires_at: Option<Instant>,
}

impl Entry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expires_at| now >= expires_at)
    }
}

#[derive(Debug)]
pub struct TtlCache {
    tables: RwLock<HashMap<String, HashMap<String, Entry>>>,
    capacity: usize,
}

impl TtlCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            tables: RwLock::new(HashMap::new()),
            capacity: capacity.max(1),
        }
    }

    pub async fn get(&self, table: &str, key: &str) -> Option<Value> {
        let now = Instant::now();
        let mut tables = self.tables.write().await;
        let entries = tables.get_mut(table)?;
        if entries.get(key).is_some_and(|entry| entry.is_expired(now)) {
            entries.remove(key);
            return None;
        }
        entries.get(key).map(|entry| entry.value.clone())
    }

    pub async fn insert(&self, table: &str, key: &str, value: Value, expires_at: Option<Instant>) {
        let mut tables = self.tables.write().await;
        let entries = tables.entry(table.to_owned()).or_default();
        if !entries.contains_key(key) && entries.len() >= self.capacity {
            make_room(entries, self.capacity);
        }
        entries.insert(
            key.to_owned(),
            Entry {
                value,
                inserted_at: Instant::now(),
                expires_at,
            },
        );
    }

    pub async fn invalidate(&self, table: &str, key: &str) {
        if let Some(entries) = self.tables.write().await.get_mut(table) {
            entries.remove(key);
        }
    }

    pub async fn invalidate_table(&self, table: &str) {
        self.tables.write().await.remove(table);
    }

    pub async fn purge_expired(&self) -> usize {
        let now = Instant::now();
        let mut purged = 0usize;
        let mut tables = self.tables.write().await;
        tables.retain(|_, entries| {
            entries.retain(|_, entry| {
                if entry.is_expired(now) {
                    purged += 1;
                    false
                } else {
                    true
                }
            });
            !entries.is_empty()
        });
        purged
    }
}

fn make_room(entries: &mut HashMap<String, Entry>, capacity: usize) {
    let now = Instant::now();
    entries.retain(|_, entry| !entry.is_expired(now));
    if entries.len() < capacity {
        return;
    }
    let oldest = entries
        .iter()
        .min_by_key(|(_, entry)| entry.inserted_at)
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest {
        entries.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn stores_reads_and_invalidates_entries() {
        let cache = TtlCache::new(8);
        assert_eq!(cache.get("t", "k").await, None);

        cache.insert("t", "k", json!(1), None).await;
        assert_eq!(cache.get("t", "k").await, Some(json!(1)));

        cache.invalidate("t", "k").await;
        assert_eq!(cache.get("t", "k").await, None);
    }

    #[tokio::test]
    async fn table_invalidation_is_scoped() {
        let cache = TtlCache::new(8);
        cache.insert("a", "k", json!(1), None).await;
        cache.insert("b", "k", json!(2), None).await;

        cache.invalidate_table("a").await;
        assert_eq!(cache.get("a", "k").await, None);
        assert_eq!(cache.get("b", "k").await, Some(json!(2)));
    }

    #[tokio::test]
    async fn expires_entries_after_ttl() {
        let cache = TtlCache::new(8);
        cache
            .insert(
                "t",
                "k",
                json!(1),
                Some(Instant::now() + Duration::from_millis(30)),
            )
            .await;
        assert_eq!(cache.get("t", "k").await, Some(json!(1)));

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(cache.get("t", "k").await, None);
    }

    #[tokio::test]
    async fn evicts_oldest_entry_when_capacity_is_reached() {
        let cache = TtlCache::new(2);
        cache.insert("t", "a", json!(1), None).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        cache.insert("t", "b", json!(2), None).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        cache.insert("t", "c", json!(3), None).await;

        assert_eq!(cache.get("t", "a").await, None);
        assert_eq!(cache.get("t", "b").await, Some(json!(2)));
        assert_eq!(cache.get("t", "c").await, Some(json!(3)));
    }

    #[tokio::test]
    async fn purge_removes_only_expired_entries() {
        let cache = TtlCache::new(8);
        cache
            .insert("t", "gone", json!(1), Some(Instant::now()))
            .await;
        cache.insert("t", "kept", json!(2), None).await;
        tokio::time::sleep(Duration::from_millis(2)).await;

        assert_eq!(cache.purge_expired().await, 1);
        assert_eq!(cache.get("t", "kept").await, Some(json!(2)));
    }
}
