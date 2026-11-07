//! Global allocator.
//!
//! The store holds millions of small, long-lived `serde_json::Value` nodes
//! next to large short-lived buffers (table scans, flush payloads, WebSocket
//! frames). glibc malloc keeps the pages freed by the short-lived ones inside
//! its per-thread arenas, so the RSS only ever grows. jemalloc returns them
//! to the OS: its background threads purge unused dirty pages on a decay
//! timer, even while the process is idle.

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Bytes the allocator reports right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocatorStats {
    /// Bytes handed out to the application.
    pub allocated: usize,
    /// Bytes of physical memory mapped by the allocator (closest to RSS).
    pub resident: usize,
}

/// Reads the allocator statistics, or `None` when they are unavailable.
#[cfg(not(target_env = "msvc"))]
pub fn stats() -> Option<AllocatorStats> {
    use tikv_jemalloc_ctl::{epoch, stats};

    // The counters are cached by jemalloc and only refreshed on an epoch bump.
    epoch::advance().ok()?;
    Some(AllocatorStats {
        allocated: stats::allocated::read().ok()?,
        resident: stats::resident::read().ok()?,
    })
}

#[cfg(target_env = "msvc")]
pub fn stats() -> Option<AllocatorStats> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_env = "msvc"))]
    fn reports_allocations() {
        let buffer = vec![1_u8; 8 * 1024 * 1024];
        let stats = stats().expect("jemalloc stats");
        assert!(stats.allocated >= buffer.len());
        assert!(stats.resident >= stats.allocated);
    }
}
