//! Per-cell telemetry capture.
//!
//! Signals recorded for each cell:
//!   - CPU and context switches via `getrusage(RUSAGE_CHILDREN)` (accurate, and
//!     it survives the child's exit).
//!   - Peak memory via the measured process's `VmHWM` (`peak_rss_bytes`), which
//!     is unprivileged and works on any host; the caller prefers the cgroup
//!     high-water mark when it has root.
//!   - Syscall occurrence counts via `perf_event_open` tracepoint counters. When
//!     a cgroup is available (run as root), the counters are cgroup-scoped
//!     (`PERF_FLAG_PID_CGROUP`, one per CPU in the cpuset), which counts every
//!     task in the leaf regardless of which thread makes the syscall or when it is
//!     created. That captures the io_threads and runtime workers that do the
//!     actual socket I/O, which per-thread enumeration missed. Without a cgroup it
//!     falls back to per-thread counters across `/proc/<pid>/task`. Both need
//!     perf (root or CAP_PERFMON, tracefs, `perf_event_paranoid <= 1`) and degrade
//!     to zero with a one-time note.
//!   - Latency is measured inside the target (REQ/REP) and reported on stdout,
//!     so the orchestrator only stores the quantiles, not a histogram.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

use perf_event_open_sys as perf;
use serde::{Deserialize, Serialize};

/// Warn once if no syscall counters could be opened, so an all-zero syscall
/// column is explained rather than silently misleading.
static PERF_WARNED: AtomicBool = AtomicBool::new(false);

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

/// Peak resident set size of a live process, in bytes, from `/proc/<pid>/status`
/// `VmHWM` (a kernel-maintained high-water mark). Unprivileged and per-process,
/// so it gives a peak-memory figure on any host, with no cgroup or root needed.
/// Read it while the process is alive (e.g. during the wait loop); once the
/// process exits, `/proc/<pid>` is gone. Returns None if the field is missing.
pub fn peak_rss_bytes(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[derive(Clone, Copy)]
enum SyscallKind {
    EpollWait,
    EpollCtl,
    SendMsg,
    RecvMsg,
    IoUringEnter,
}

/// Per-syscall tracepoint counters for a process. The socket syscalls of these
/// engines run on background threads (libzmq's io_threads, the async runtimes'
/// workers), not the process's first thread, so a single PID-scoped counter
/// misses them. This opens a counter on every thread in `/proc/<pid>/task` and
/// also sets `inherit`, so threads that exist when the probe attaches are counted
/// directly and threads spawned later are counted through inheritance; `read()`
/// sums across all of them. Each per-thread fd retains its count after that
/// thread exits, so the sum is correct even though the threads are gone by the
/// time the process is reaped.
///
/// Open it after the target has spun up its threads, then `read()` while the fds
/// are open. Best-effort: counters that cannot be opened (no tracefs, paranoid
/// too high, not root) are skipped and read back as zero.
pub struct SyscallProbe {
    counters: Vec<(SyscallKind, i32)>,
}

const WANT: [(SyscallKind, &str); 5] = [
    (SyscallKind::EpollWait, "sys_enter_epoll_wait"),
    (SyscallKind::EpollCtl, "sys_enter_epoll_ctl"),
    (SyscallKind::SendMsg, "sys_enter_sendmsg"),
    (SyscallKind::RecvMsg, "sys_enter_recvmsg"),
    (SyscallKind::IoUringEnter, "sys_enter_io_uring_enter"),
];

impl SyscallProbe {
    /// Scope counters to a cgroup with `PERF_FLAG_PID_CGROUP`. This is the robust
    /// path: a cgroup-scoped counter counts every task in the cgroup (all threads,
    /// however and whenever they are created), so it captures the syscalls that
    /// libzmq's io_threads and the async runtimes' workers make, which per-thread
    /// enumeration kept missing. cgroup mode requires a per-CPU counter, so one is
    /// opened per CPU in the cell's cpuset and summed. Falls back to the per-thread
    /// path if the cgroup counters cannot be opened.
    pub fn open_cgroup(cgroup_path: &std::path::Path, cpus: &[u32], pid: u32) -> Self {
        let mut counters = Vec::new();
        if let Some(cgfd) = open_cgroup_fd(cgroup_path) {
            for &cpu in cpus {
                for (kind, tp) in WANT {
                    if let Some(fd) = open_tracepoint_cgroup(cgfd, cpu, tp) {
                        counters.push((kind, fd));
                    }
                }
            }
            // SAFETY: cgfd is an fd we opened; the perf events keep their own
            // reference to the cgroup, so closing the directory fd is safe.
            unsafe {
                libc::close(cgfd);
            }
        }
        if !counters.is_empty() {
            return SyscallProbe { counters };
        }
        // cgroup scoping unavailable; fall back to per-thread enumeration.
        Self::open(pid)
    }

    pub fn open(pid: u32) -> Self {
        let mut counters = Vec::new();
        for tid in thread_ids(pid) {
            for (kind, tp) in WANT {
                if let Some(fd) = open_tracepoint(tid, tp) {
                    counters.push((kind, fd));
                }
            }
        }
        if counters.is_empty() && !PERF_WARNED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "  syscall counting unavailable (need root or CAP_PERFMON, tracefs \
                 mounted, and perf_event_paranoid <= 1); recording 0. Run with sudo \
                 (make run-root) for these counts."
            );
        }
        SyscallProbe { counters }
    }

    pub fn read(&self) -> SyscallCounters {
        let mut c = SyscallCounters::default();
        // Sum across every per-thread counter; one kind has one fd per thread.
        for (kind, fd) in &self.counters {
            let v = read_counter(*fd);
            match kind {
                SyscallKind::EpollWait => c.epoll_wait += v,
                SyscallKind::EpollCtl => c.epoll_ctl += v,
                SyscallKind::SendMsg => c.sendmsg += v,
                SyscallKind::RecvMsg => c.recvmsg += v,
                SyscallKind::IoUringEnter => c.io_uring_enter += v,
            }
        }
        c
    }
}

/// Thread ids of a process from `/proc/<pid>/task`, falling back to the pid
/// itself if the directory cannot be read.
fn thread_ids(pid: u32) -> Vec<u32> {
    let mut tids = Vec::new();
    if let Ok(entries) = fs::read_dir(format!("/proc/{pid}/task")) {
        for e in entries.flatten() {
            if let Ok(tid) = e.file_name().to_string_lossy().parse::<u32>() {
                tids.push(tid);
            }
        }
    }
    if tids.is_empty() {
        tids.push(pid);
    }
    tids
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
    // inherit: count the threads and children the target spawns after the probe
    // attaches, not just its main thread. This is essential here, because the
    // actual socket syscalls run on libzmq's io_threads and on the async
    // runtimes' worker threads, not on the process's first thread. The probe is
    // opened immediately after spawn, before the target builds its runtime, so
    // those threads are inherited.
    attr.set_inherit(1);
    // SAFETY: attr is fully initialized; pid scoping with cpu=-1 counts the task
    // across all CPUs. Returns a non-negative fd on success.
    let fd = unsafe { perf::perf_event_open(&mut attr, pid as i32, -1, -1, 0) };
    if fd < 0 {
        None
    } else {
        Some(fd)
    }
}

/// Open the cgroup leaf directory so it can be passed as the "pid" argument of a
/// cgroup-scoped perf event.
fn open_cgroup_fd(path: &std::path::Path) -> Option<i32> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: c is a valid NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if fd < 0 { None } else { Some(fd) }
}

/// Open a tracepoint counter scoped to a cgroup on one CPU: pid is the cgroup fd,
/// cpu is a real CPU, and the cgroup flag selects cgroup scoping. No `inherit`,
/// since the cgroup already covers every task.
fn open_tracepoint_cgroup(cgroup_fd: i32, cpu: u32, name: &str) -> Option<i32> {
    let id = tracepoint_id(name)?;
    // SAFETY: a zeroed perf_event_attr is valid; we set only documented fields.
    let mut attr: perf::bindings::perf_event_attr = unsafe { std::mem::zeroed() };
    attr.type_ = 2; // PERF_TYPE_TRACEPOINT
    attr.size = std::mem::size_of::<perf::bindings::perf_event_attr>() as u32;
    attr.config = id;
    // SAFETY: attr is initialized; cgroup_fd is a directory fd; cpu is valid.
    let fd = unsafe {
        perf::perf_event_open(
            &mut attr,
            cgroup_fd,
            cpu as i32,
            -1,
            perf::bindings::PERF_FLAG_PID_CGROUP as u64,
        )
    };
    if fd < 0 { None } else { Some(fd) }
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
