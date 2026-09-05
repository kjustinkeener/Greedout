//! System stats (per-core CPU + memory) for the optional bottom status bar.
//!
//! One `System` instance lives in the poll loop (see lib.rs) and is refreshed
//! once per tick. CPU usage needs a short interval between refreshes to be
//! meaningful; the poll cadence (>=0.1s, default 0.5s) comfortably exceeds
//! sysinfo's minimum, so a plain per-tick refresh is fine.

use serde::Serialize;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// One sample of system load, emitted on the "sysstats" event when the status
/// bar is enabled.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SysStats {
    /// Per-core CPU usage, 0..100, in core order.
    pub cpus: Vec<f32>,
    /// Used memory in bytes.
    pub mem_used: u64,
    /// Total memory in bytes.
    pub mem_total: u64,
    /// Used / total, 0..1.
    pub mem_pct: f32,
}

/// Wraps the persistent `System`. Kept alive across polls so per-core CPU deltas
/// are computed against the previous refresh.
pub struct StatsSampler {
    sys: System,
}

impl StatsSampler {
    pub fn new() -> Self {
        // Only refresh CPU + memory; skip the (expensive) process list.
        let sys = System::new_with_specifics(
            RefreshKind::new()
                .with_cpu(CpuRefreshKind::new().with_cpu_usage())
                .with_memory(MemoryRefreshKind::new().with_ram()),
        );
        StatsSampler { sys }
    }

    /// Refresh and return the current sample.
    pub fn sample(&mut self) -> SysStats {
        self.sys
            .refresh_cpu_specifics(CpuRefreshKind::new().with_cpu_usage());
        self.sys
            .refresh_memory_specifics(MemoryRefreshKind::new().with_ram());

        let cpus: Vec<f32> = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        let mem_total = self.sys.total_memory();
        let mem_used = self.sys.used_memory();
        let mem_pct = if mem_total > 0 {
            mem_used as f32 / mem_total as f32
        } else {
            0.0
        };
        SysStats {
            cpus,
            mem_used,
            mem_total,
            mem_pct,
        }
    }
}
