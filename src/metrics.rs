use std::sync::atomic::{AtomicU64, Ordering};

use tracing::info;

use crate::{alloc, db::MemoryStats};

const MIB: f64 = 1024.0 * 1024.0;

#[derive(Debug, Default)]
pub struct Metrics {
    total_connections: AtomicU64,
    active_connections: AtomicU64,
    requests_total: AtomicU64,
    requests_failed: AtomicU64,
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

    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }

    pub fn total_requests(&self) -> u64 {
        self.requests_total.load(Ordering::Relaxed)
    }

    pub fn log_stats(&self, active_sessions: usize, memory: MemoryStats) {
        let total_requests = self.requests_total.load(Ordering::Relaxed);
        let failed_requests = self.requests_failed.load(Ordering::Relaxed);

        #[allow(clippy::cast_precision_loss)]
        let (success_rate, memory_hit_rate) = {
            let success_rate = if total_requests == 0 {
                100.0
            } else {
                ((total_requests - failed_requests) as f64 / total_requests as f64) * 100.0
            };
            let lookups = memory.hits + memory.misses;
            let memory_hit_rate = if lookups == 0 {
                0.0
            } else {
                (memory.hits as f64 / lookups as f64) * 100.0
            };
            (success_rate, memory_hit_rate)
        };

        info!(
            active_connections = self.active_connections(),
            total_connections = self.total_connections(),
            active_sessions,
            total_requests = self.total_requests(),
            success_rate = format!("{success_rate:.2}%"),
            memory_hit_rate = format!("{memory_hit_rate:.2}%"),
            memory_entries = memory.entries,
            memory_evicted = memory.evicted,
            memory_packed = memory.packed,
            "server stats"
        );
        if let Some(allocator) = alloc::stats() {
            #[allow(clippy::cast_precision_loss)]
            let (allocated, resident) = (
                allocator.allocated as f64 / MIB,
                allocator.resident as f64 / MIB,
            );
            info!(
                allocated = format!("{allocated:.1} MiB"),
                resident = format!("{resident:.1} MiB"),
                "memory stats"
            );
        }
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

        metrics.log_stats(
            3,
            MemoryStats {
                hits: 8,
                misses: 2,
                entries: 5,
                evicted: 1,
                packed: 2,
            },
        );
    }
}
