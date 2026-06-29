//! Kernel-boundary telemetry capture.
//!
//! Primary metrics are taken from the kernel, not from user-space timers inside
//! the target. The orchestrator drives three independent capture paths:
//!
//!   1. Latency  : per-message round-trip recorded into an [`HdrLatency`]
//!                 histogram (p50/p90/p99/p99.9). Timestamps are captured by
//!                 the orchestrator at the measurement boundary.
//!   2. Syscalls : exact occurrence counts of `epoll_ctl`, `epoll_wait`,
//!                 `sendmsg`, `recvmsg`, `io_uring_enter` via eBPF tracepoints
//!                 (feature `ebpf`) or perf_event_open (feature `perf`).
//!   3. Scheduling: voluntary / involuntary context switches from
//!                 `/proc/[pid]/schedstat` or `getrusage`.
//!
//! The syscall and perf integrations are feature-gated stubs. Their public
//! shapes are stable; the capture bodies are left to implementation so the base
//! build needs no privileged counters. Counter field names below mirror the
//! kernel tracepoint names exactly to avoid an attribution mismatch.

use std::fs;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

/// Kernel USER_HZ. 100 on essentially all Linux builds; used to convert the
/// clock ticks in /proc/<pid>/stat to seconds. If a host uses a different
/// CONFIG_HZ, read it via sysconf(_SC_CLK_TCK) instead.
const USER_HZ: f64 = 100.0;

/// Exact syscall occurrence counts over one measurement block, attributed to a
/// single target PID (and its children).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyscallCounters {
    pub epoll_ctl: u64,
    pub epoll_wait: u64,
    pub sendmsg: u64,
    pub recvmsg: u64,
    pub io_uring_enter: u64,
}

/// Context-switch deltas captured across the measurement block.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SchedCounters {
    /// Voluntary switches: the task blocked (e.g. waiting on a socket).
    pub voluntary_ctxt_switches: u64,
    /// Involuntary switches: the scheduler preempted the task. High counts on a
    /// pinned, isolated core indicate noise that contaminates the tail.
    pub involuntary_ctxt_switches: u64,
}

/// Latency histogram wrapper. Three significant figures gives sub-percent error
/// out to the p99.9 tail the spec targets.
pub struct HdrLatency {
    hist: Histogram<u64>,
}

impl HdrLatency {
    pub fn new() -> anyhow::Result<Self> {
        // sigfig = 3 -> values stored with 0.1% relative quantization error.
        let hist = Histogram::<u64>::new(3)?;
        Ok(Self { hist })
    }

    /// Record one latency sample in nanoseconds.
    pub fn record_ns(&mut self, ns: u64) -> anyhow::Result<()> {
        self.hist.record(ns)?;
        Ok(())
    }

    /// Snapshot the quantiles of interest into a serializable record.
    pub fn snapshot(&self) -> LatencySnapshot {
        LatencySnapshot {
            count: self.hist.len(),
            min_ns: self.hist.min(),
            max_ns: self.hist.max(),
            p50_ns: self.hist.value_at_quantile(0.50),
            p90_ns: self.hist.value_at_quantile(0.90),
            p99_ns: self.hist.value_at_quantile(0.99),
            p999_ns: self.hist.value_at_quantile(0.999),
        }
    }
}

/// Steady-state throughput over one measurement block.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Throughput {
    pub msgs_per_s: f64,
    pub mbps: f64,
}

/// Serializable latency quantiles for the run record / RANKING.md generator.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatencySnapshot {
    pub count: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub p50_ns: u64,
    pub p90_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
}

/// Owns the active kernel counters for one measurement block. `start` arms the
/// probes; `stop` disarms and returns the deltas. The target PID scopes the
/// attribution so counts from the orchestrator or unrelated processes are
/// excluded.
pub struct TelemetrySession {
    target_pid: u32,
}

impl TelemetrySession {
    pub fn start(target_pid: u32) -> anyhow::Result<Self> {
        // TODO(impl, feature=ebpf): attach tracepoint programs filtered on
        // target_pid for the five syscalls in SyscallCounters.
        // TODO(impl, feature=perf): perf_event_open per counter, scoped to pid.
        Ok(Self { target_pid })
    }

    /// Disarm and collect. Context switches come from `/proc/<pid>/status`
    /// (implemented below). Syscall counts still require the eBPF/perf path
    /// (feature `ebpf`/`perf`); until that is wired they are returned as zero.
    ///
    /// Read this before reaping the target: once the process exits, its
    /// `/proc/<pid>` entry disappears and the counters read back as zero.
    pub fn stop(self) -> anyhow::Result<(SyscallCounters, SchedCounters)> {
        // TODO(impl, feature=ebpf/perf): drain the syscall counter maps here.
        let sched = read_sched(self.target_pid);
        Ok((SyscallCounters::default(), sched))
    }
}

/// Voluntary / involuntary context switches for a live PID, parsed from
/// `/proc/<pid>/status`. Best-effort: returns zeros if the process is gone or
/// the fields are missing.
pub fn read_sched(pid: u32) -> SchedCounters {
    let mut out = SchedCounters::default();
    let path = format!("/proc/{pid}/status");
    let Ok(text) = fs::read_to_string(&path) else {
        return out;
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
            out.voluntary_ctxt_switches = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
            out.involuntary_ctxt_switches = v.trim().parse().unwrap_or(0);
        }
    }
    out
}

/// CPU seconds (user + system) consumed by a live PID, from the `utime`/`stime`
/// fields of `/proc/<pid>/stat`. Best-effort: 0.0 if unavailable.
///
/// The `comm` field can contain spaces and parentheses, so the numeric fields
/// are taken after the final ')'. After that, field 3 (state) is index 0, so
/// utime (field 14) is index 11 and stime (field 15) is index 12.
pub fn read_cpu_seconds(pid: u32) -> f64 {
    let path = format!("/proc/{pid}/stat");
    let Ok(text) = fs::read_to_string(&path) else {
        return 0.0;
    };
    let Some(close) = text.rfind(')') else {
        return 0.0;
    };
    let rest = text[close + 1..].trim_start();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
    (utime + stime) as f64 / USER_HZ
}

/// Cumulative CPU seconds and context switches across all reaped child
/// processes, from `getrusage(RUSAGE_CHILDREN)`. Snapshot before and after a
/// cell and take the delta; only children that have been `wait()`ed are counted,
/// which `std::process::Child::wait` guarantees. Accurate and survives the
/// child's exit, unlike the `/proc` reads above.
pub fn rusage_children() -> (f64, SchedCounters) {
    // SAFETY: getrusage writes into a zeroed, correctly-typed rusage struct.
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_CHILDREN, &mut ru) != 0 {
            return (0.0, SchedCounters::default());
        }
        let cpu = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1e6
            + ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1e6;
        let sched = SchedCounters {
            voluntary_ctxt_switches: ru.ru_nvcsw.max(0) as u64,
            involuntary_ctxt_switches: ru.ru_nivcsw.max(0) as u64,
        };
        (cpu, sched)
    }
}
