//! What machine actually ran the benchmark, sampled rather than asserted.
//!
//! The run's provenance used to be a pair of command-line strings the operator
//! typed at render time. That is the one piece of a benchmark that must not be
//! taken on trust: the render step can run on a different machine than the
//! measurement, and "turbo off, C-states locked" is exactly the claim someone
//! pastes into a flag without checking. Everything here is read out of /proc and
//! /sys on the box doing the measuring, at the moment it measures.
//!
//! The point is not decoration. `admissible` is derived from these facts, so
//! whether a run counts as a real comparison is decided by the machine's actual
//! configuration instead of by whoever is writing the summary.

use std::fs;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Host {
    /// CPU model string from /proc/cpuinfo.
    pub cpu: String,
    /// Online logical CPUs.
    pub cpu_count: usize,
    /// Kernel release, which decides `io_uring` behaviour among other things.
    pub kernel: String,
    /// Total RAM in kibibytes.
    pub memory_total_kb: u64,
    /// cpufreq governor, when every online CPU agrees on one. `None` when the
    /// CPUs disagree or cpufreq is not exposed, which is itself worth seeing.
    pub governor: Option<String>,
    /// Turbo / boost state: `Some(true)` when it is on, `Some(false)` when the
    /// kernel reports it disabled, `None` when neither knob is present.
    pub turbo: Option<bool>,
    /// Whether the CPU reports the hypervisor flag. A guest cannot pin frequency
    /// or see its neighbours, so this alone disqualifies a run as a verdict.
    pub virtualized: bool,
    /// Root gets cgroup pinning and cgroup-scoped perf counters; without it the
    /// isolation the matrix asks for is not actually applied.
    pub root: bool,
    /// Derived, never typed: a run is admissible as a comparison only on bare
    /// metal, with the performance governor and turbo disabled, as root.
    pub admissible: bool,
    /// Why not, in the reader's words, when `admissible` is false.
    pub inadmissible_reasons: Vec<String>,
    /// One-line summary for the dashboard's provenance strip. Written here, from
    /// the facts above, so no reader has to re-derive it and reach a different
    /// conclusion than the flags it replaced.
    pub note: String,
}

fn first_line_after(path: &str, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Governor shared by every online CPU, or None if they differ. Reading only
/// cpu0 would miss a machine where one core was left on `powersave`.
fn uniform_governor(cpu_count: usize) -> Option<String> {
    let mut seen: Option<String> = None;
    for i in 0..cpu_count {
        let g = read_trimmed(&format!(
            "/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_governor"
        ))?;
        match &seen {
            None => seen = Some(g),
            Some(prev) if *prev == g => {}
            Some(_) => return None,
        }
    }
    seen
}

/// Turbo across the two kernel interfaces that expose it. `intel_pstate` inverts
/// the sense (`no_turbo=1` means turbo is off), which is easy to read backwards.
fn turbo_enabled() -> Option<bool> {
    if let Some(v) = read_trimmed("/sys/devices/system/cpu/intel_pstate/no_turbo") {
        return Some(v != "1");
    }
    if let Some(v) = read_trimmed("/sys/devices/system/cpu/cpufreq/boost") {
        return Some(v == "1");
    }
    None
}

impl Host {
    /// Sample the machine. Every field degrades to a neutral value rather than
    /// failing: a run on a host that hides `cpufreq` should still record what it
    /// could see, and say the rest is unknown.
    pub fn probe() -> Self {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let cpu = first_line_after("/proc/cpuinfo", "model name")
            .unwrap_or_else(|| "unknown CPU".to_string());
        let cpu_count = cpuinfo
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count()
            .max(1);
        let kernel =
            read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".to_string());
        let memory_total_kb = first_line_after("/proc/meminfo", "MemTotal")
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0);
        let virtualized = cpuinfo
            .lines()
            .any(|l| l.starts_with("flags") && l.contains(" hypervisor"));
        let governor = uniform_governor(cpu_count);
        let turbo = turbo_enabled();
        // SAFETY: geteuid cannot fail and touches no memory we own.
        let root = unsafe { libc::geteuid() } == 0;

        let mut reasons = Vec::new();
        if virtualized {
            reasons.push("runs in a VM, so frequency and neighbours are not controlled".into());
        }
        match &governor {
            Some(g) if g == "performance" => {}
            Some(g) => reasons.push(format!("cpufreq governor is {g}, not performance")),
            None => reasons.push("cpufreq governor is not uniform or not readable".into()),
        }
        match turbo {
            Some(false) => {}
            Some(true) => reasons.push("turbo/boost is enabled, so clocks are not pinned".into()),
            None => reasons.push("turbo/boost state is not readable".into()),
        }
        if !root {
            reasons.push("not run as root, so cgroup pinning and perf counters are absent".into());
        }

        let note = if reasons.is_empty() {
            "bare metal, performance governor, turbo disabled, run as root".to_string()
        } else {
            format!("not admissible: {}", reasons.join("; "))
        };

        Self {
            cpu,
            cpu_count,
            kernel,
            memory_total_kb,
            governor,
            turbo,
            virtualized,
            root,
            admissible: reasons.is_empty(),
            inadmissible_reasons: reasons,
            note,
        }
    }
}
