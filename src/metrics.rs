use std::sync::atomic::{AtomicU64, Ordering};

use tracing::info;

#[derive(Debug, Default)]
pub struct Metrics {
    total_connections: AtomicU64,
    active_connections: AtomicU64,
    requests_total: AtomicU64,
    requests_failed: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

impl Metrics {
    pub fn connection_opened(&self) {
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn request_started(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn request_failed(&self) {
        self.requests_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }

    pub fn total_requests(&self) -> u64 {
        self.requests_total.load(Ordering::Relaxed)
    }

    pub fn log_stats(&self, active_sessions: usize) {
        let total_requests = self.requests_total.load(Ordering::Relaxed);
        let failed_requests = self.requests_failed.load(Ordering::Relaxed);
        let cache_hits = self.cache_hits.load(Ordering::Relaxed);
        let cache_misses = self.cache_misses.load(Ordering::Relaxed);

        #[allow(clippy::cast_precision_loss)]
        let (success_rate, cache_hit_rate) = {
            let success_rate = if total_requests == 0 {
                100.0
            } else {
                ((total_requests - failed_requests) as f64 / total_requests as f64) * 100.0
            };
            let lookups = cache_hits + cache_misses;
            let cache_hit_rate = if lookups == 0 {
                0.0
            } else {
                (cache_hits as f64 / lookups as f64) * 100.0
            };
            (success_rate, cache_hit_rate)
        };

        info!(
            active_connections = self.active_connections(),
            total_connections = self.total_connections(),
            active_sessions,
            total_requests = self.total_requests(),
            success_rate = format!("{success_rate:.2}%"),
            cache_hit_rate = format!("{cache_hit_rate:.2}%"),
            "server stats"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_counters_track_lifecycle() {
        let metrics = Metrics::default();
        assert_eq!(metrics.active_connections(), 0);

        metrics.connection_opened();
        metrics.connection_opened();
        assert_eq!(metrics.active_connections(), 2);

        metrics.connection_closed();
        assert_eq!(metrics.active_connections(), 1);

        metrics.log_stats(3);
    }
}
