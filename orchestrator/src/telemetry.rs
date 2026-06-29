//! Per-cell telemetry capture.
//!
//! Three signals are recorded for each cell:
//!   - CPU and context switches via `getrusage(RUSAGE_CHILDREN)` (accurate, and
//!     it survives the child's exit).
//!   - Syscall occurrence counts via `perf_event_open` tracepoint counters
//!     scoped to the measured PID. These count only when the host permits it
//!     (root, tracefs mounted, `perf_event_paranoid <= 1`) and degrade to zero
//!     otherwise.
//!   - Latency is measured inside the target (REQ/REP) and reported on stdout,
//!     so the orchestrator only stores the quantiles, not a histogram.

use std::fs;

use perf_event_open_sys as perf;
use serde::{Deserialize, Serialize};

/// Exact syscall occurrence counts over one measurement block, scoped to the
/// measured PID.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyscallCounters {
    pub epoll_ctl: u64,
    pub epoll_wait: u64,
    pub sendmsg: u64,
    pub recvmsg: u64,
    pub io_uring_enter: u64,
}

/// Context-switch deltas across the measurement block.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SchedCounters {
    /// Voluntary switches: the task blocked (e.g. waiting on a socket).
    pub voluntary_ctxt_switches: u64,
    /// Involuntary switches: the scheduler preempted the task. High counts on a
    /// pinned, isolated core indicate noise that contaminates the tail.
    pub involuntary_ctxt_switches: u64,
}

/// Steady-state throughput over one measurement block.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Throughput {
    pub msgs_per_s: f64,
    pub mbps: f64,
}

/// Serializable latency quantiles (nanoseconds) for the run record.
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

/// Cumulative CPU seconds and context switches across all reaped child
/// processes, from `getrusage(RUSAGE_CHILDREN)`. Snapshot before and after a
/// cell and take the delta; only children that have been `wait()`ed are counted,
/// which `std::process::Child::wait` guarantees.
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

#[derive(Clone, Copy)]
enum SyscallKind {
    EpollWait,
    EpollCtl,
    SendMsg,
    RecvMsg,
    IoUringEnter,
}

/// Per-syscall tracepoint counters scoped to one PID. Open it right after
/// spawning the target, then `read()` before reaping while the fds are open.
/// Best-effort: counters that cannot be opened (no tracefs, paranoid too high,
/// not root) are skipped and read back as zero.
pub struct SyscallProbe {
    counters: Vec<(SyscallKind, i32)>,
}

impl SyscallProbe {
    pub fn open(pid: u32) -> Self {
        let want = [
            (SyscallKind::EpollWait, "sys_enter_epoll_wait"),
            (SyscallKind::EpollCtl, "sys_enter_epoll_ctl"),
            (SyscallKind::SendMsg, "sys_enter_sendmsg"),
            (SyscallKind::RecvMsg, "sys_enter_recvmsg"),
            (SyscallKind::IoUringEnter, "sys_enter_io_uring_enter"),
        ];
        let mut counters = Vec::new();
        for (kind, tp) in want {
            if let Some(fd) = open_tracepoint(pid, tp) {
                counters.push((kind, fd));
            }
        }
        SyscallProbe { counters }
    }

    pub fn read(&self) -> SyscallCounters {
        let mut c = SyscallCounters::default();
        for (kind, fd) in &self.counters {
            let v = read_counter(*fd);
            match kind {
                SyscallKind::EpollWait => c.epoll_wait = v,
                SyscallKind::EpollCtl => c.epoll_ctl = v,
                SyscallKind::SendMsg => c.sendmsg = v,
                SyscallKind::RecvMsg => c.recvmsg = v,
                SyscallKind::IoUringEnter => c.io_uring_enter = v,
            }
        }
        c
    }
}

impl Drop for SyscallProbe {
    fn drop(&mut self) {
        for (_, fd) in &self.counters {
            // SAFETY: fd is a perf_event fd we opened and still own.
            unsafe {
                libc::close(*fd);
            }
        }
    }
}

fn tracepoint_id(name: &str) -> Option<u64> {
    for base in [
        "/sys/kernel/tracing/events/syscalls/",
        "/sys/kernel/debug/tracing/events/syscalls/",
    ] {
        let path = format!("{base}{name}/id");
        if let Ok(s) = fs::read_to_string(&path) {
            if let Ok(id) = s.trim().parse::<u64>() {
                return Some(id);
            }
        }
    }
    None
}

fn open_tracepoint(pid: u32, name: &str) -> Option<i32> {
    let id = tracepoint_id(name)?;
    // SAFETY: a zeroed perf_event_attr is a valid struct; we set only the
    // documented fields and leave the rest zero.
    let mut attr: perf::bindings::perf_event_attr = unsafe { std::mem::zeroed() };
    attr.type_ = 2; // PERF_TYPE_TRACEPOINT
    attr.size = std::mem::size_of::<perf::bindings::perf_event_attr>() as u32;
    attr.config = id;
    // SAFETY: attr is fully initialized; pid scoping with cpu=-1 counts the task
    // across all CPUs. Returns a non-negative fd on success.
    let fd = unsafe { perf::perf_event_open(&mut attr, pid as i32, -1, -1, 0) };
    if fd < 0 {
        None
    } else {
        Some(fd)
    }
}

fn read_counter(fd: i32) -> u64 {
    let mut v: u64 = 0;
    // SAFETY: a perf_event counter fd yields a u64 count on read.
    let n = unsafe { libc::read(fd, (&mut v as *mut u64).cast::<libc::c_void>(), 8) };
    if n == 8 {
        v
    } else {
        0
    }
}
