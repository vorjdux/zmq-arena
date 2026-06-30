//! zmq-arena control plane (orchestrator).
//!
//! Responsibilities:
//!   1. Load the benchmark matrix.
//!   2. For each cell, provision two cgroup v2 leaves (publisher, subscriber),
//!      set up the transport (netns TCP or IPC), and spawn the two target
//!      binaries as distinct OS processes.
//!   3. Arm kernel telemetry, run the measurement block, disarm, collect.
//!   4. Emit a structured JSON record per cell for the RANKING.md generator and
//!      the /history archive.
//!
//! The throughput kind (PUSH/PULL over ipc and loopback tcp) is implemented and
//! runs the target as two real processes; cgroup isolation is applied when run
//! as root and skipped gracefully otherwise. netns, eBPF syscall counting, and
//! the latency/pubsub/fanout/fanin kinds are not yet wired. `run --dry-run`
//! works unprivileged.

mod cgroups;
mod config;
mod telemetry;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use crate::cgroups::Cgroup;
use crate::config::{Isolation, Kind, MatrixEntry, RunConfig, Transport};
use crate::telemetry::{LatencySnapshot, SchedCounters, SyscallCounters, Throughput};

#[derive(Parser)]
#[command(name = "zmq-arena", version, about = "ZMTP benchmarking arena control plane")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute a matrix run (or print the expanded plan with --dry-run).
    Run(RunArgs),
}

#[derive(clap::Args)]
struct RunArgs {
    /// Path to the matrix definition (JSON).
    #[arg(long, default_value = "matrix.json")]
    matrix: PathBuf,
    /// Stable identifier for this run, used in cgroup paths and the archive
    /// filename. Defaults to the UTC date the weekly grid expects.
    #[arg(long, default_value = "manual")]
    run_id: String,
    /// Directory for per-cell JSON records. Defaults to ./scratch.
    #[arg(long, default_value = "scratch")]
    out: PathBuf,
    /// Expand and print the plan without provisioning cgroups or spawning
    /// targets. Safe to run unprivileged.
    #[arg(long)]
    dry_run: bool,
}

/// One serialized measurement cell: the input cell plus all captured telemetry.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CellRecord {
    run_id: String,
    cell_id: String,
    entry: MatrixEntry,
    /// Classification and library version the target reported via `describe`.
    #[serde(default)]
    meta: TargetMeta,
    latency: LatencySnapshot,
    throughput: Throughput,
    cpu_seconds: f64,
    syscalls: SyscallCounters,
    sched: SchedCounters,
    peak_memory_bytes: u64,
}

/// Self-reported target classification. The target is the source of truth: the
/// orchestrator runs `<binary> describe` once per binary and embeds the parsed
/// JSON in every record, so library version evolution tracks automatically.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TargetMeta {
    #[serde(default)]
    engine: String,
    #[serde(default)]
    lib_version: String,
    #[serde(default)]
    binding_version: Option<String>,
    #[serde(default)]
    lib_language: String,
    /// "native" or "ffi".
    #[serde(default, rename = "impl")]
    impl_: String,
    /// Language the FFI binds into; null when native.
    #[serde(default)]
    ffi_to: Option<String>,
    #[serde(default)]
    language: String,
    /// "sync" or "async".
    #[serde(default)]
    concurrency: String,
    #[serde(default)]
    threading: String,
    #[serde(default)]
    io: String,
}

/// Run `<binary> describe` and parse the one-line JSON classification, caching by
/// binary path. On any failure, fall back to a minimal record carrying the target
/// id as the engine, so a target without a `describe` mode still produces a
/// well-formed (if sparse) record rather than aborting the cell.
fn target_meta(binary: &Path, fallback_id: &str) -> TargetMeta {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, TargetMeta>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(m) = cache.lock().unwrap().get(binary) {
        return m.clone();
    }
    let meta = ProcCommand::new(binary)
        .arg("describe")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<TargetMeta>(&o.stdout).ok())
        .unwrap_or_else(|| {
            eprintln!("  note: {} has no usable `describe`; recording id only", binary.display());
            TargetMeta { engine: fallback_id.to_string(), ..Default::default() }
        });
    cache.lock().unwrap().insert(binary.to_path_buf(), meta.clone());
    meta
}

fn cell_id(entry: &MatrixEntry, index: usize) -> String {
    format!(
        "{}-{:?}-{:?}-{}b-p{}-{:03}",
        entry.target.id, entry.transport, entry.kind,
        entry.payload_bytes, entry.peers.unwrap_or(0), index
    )
    .to_lowercase()
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args),
    }
}

fn run(args: RunArgs) -> anyhow::Result<()> {
    let cfg = RunConfig::load(&args.matrix)
        .with_context(|| format!("loading matrix {}", args.matrix.display()))?;

    eprintln!(
        "zmq-arena: run_id={} cells={} dry_run={}",
        args.run_id,
        cfg.entries.len(),
        args.dry_run
    );

    if !args.dry_run {
        std::fs::create_dir_all(&args.out)
            .with_context(|| format!("creating output dir {}", args.out.display()))?;
    }

    for (i, entry) in cfg.entries.iter().enumerate() {
        let id = cell_id(entry, i);

        if args.dry_run {
            eprintln!(
                "  plan {id}: target={} variant={} transport={:?} kind={:?} peers={:?} payload={}B msgs={} (knobs: {})",
                entry.target.id,
                entry.target.variant.as_deref().unwrap_or("default"),
                entry.transport,
                entry.kind,
                entry.peers,
                entry.payload_bytes,
                entry.messages,
                format_knobs(entry),
            );
            continue;
        }

        match execute_cell(&args.run_id, &id, entry, &cfg.isolation) {
            Ok(record) => {
                let out_path = args.out.join(format!("{id}.json"));
                std::fs::write(&out_path, serde_json::to_vec_pretty(&record)?)
                    .with_context(|| format!("writing record {}", out_path.display()))?;
                eprintln!("  done {id} -> {}", out_path.display());
            }
            // One bad cell should not abort the grid.
            Err(e) => eprintln!("  skip {id}: {e:#}"),
        }
    }

    Ok(())
}

fn format_knobs(entry: &MatrixEntry) -> String {
    if entry.target.knobs.is_empty() {
        return "default".into();
    }
    entry
        .target
        .knobs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Execute one cell. The runnable path currently supports the `throughput`
/// kind (PUSH/PULL) over `ipc` and loopback `tcp`. Other kinds return an error
/// so the run loop skips them without aborting the grid.
fn execute_cell(
    run_id: &str,
    cell_id: &str,
    entry: &MatrixEntry,
    isolation: &Isolation,
) -> anyhow::Result<CellRecord> {
    let mut record = match entry.kind {
        Kind::Throughput => run_throughput(run_id, cell_id, entry, isolation),
        Kind::Latency => run_latency(run_id, cell_id, entry, isolation),
        // pubsub and fanout: the producer is the coordinator and binds.
        Kind::PubSub | Kind::FanOut => run_multipeer(run_id, cell_id, entry, isolation, true),
        // fanin: the single consumer (the sink) is the coordinator and binds.
        Kind::FanIn => run_multipeer(run_id, cell_id, entry, isolation, false),
    }?;
    record.meta = target_meta(&entry.target.binary, &entry.target.id);
    Ok(record)
}

/// Args for a multi-peer cell: the unified set plus the bind side and the
/// duration window the duration-based kinds use.
fn peer_args(entry: &MatrixEntry, role: &str, endpoint: &str, bind: bool, duration: f64) -> Vec<String> {
    let mut a = target_args(entry, role, endpoint);
    if bind {
        a.push("--bind".into());
    }
    a.push("--duration-secs".into());
    a.push(format!("{duration}"));
    a
}

/// Parse a `THROUGHPUT <count> <elapsed_secs>` line from a measured consumer.
fn parse_throughput_line(out: &str) -> Option<(u64, f64)> {
    for line in out.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() >= 3 && t[0] == "THROUGHPUT" {
            return Some((t[1].parse().ok()?, t[2].parse().ok()?));
        }
    }
    None
}

/// Parse the REQ client's `LATENCY <count> <min> <p50> <p90> <p99> <p999> <max>`
/// line (nanoseconds) into a snapshot.
fn parse_latency(out: &str) -> Option<LatencySnapshot> {
    for line in out.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() >= 8 && t[0] == "LATENCY" {
            let n = |i: usize| t[i].parse::<u64>().ok();
            return Some(LatencySnapshot {
                count: n(1)?,
                min_ns: n(2)?,
                p50_ns: n(3)?,
                p90_ns: n(4)?,
                p99_ns: n(5)?,
                p999_ns: n(6)?,
                max_ns: n(7)?,
            });
        }
    }
    None
}

/// Build the CLI args every wrapper accepts, for one role and endpoint.
fn target_args(entry: &MatrixEntry, role: &str, endpoint: &str) -> Vec<String> {
    let transport = match entry.transport {
        Transport::TcpNetns => "tcp",
        Transport::Ipc => "ipc",
        Transport::Inproc => "inproc",
    };
    let kind = format!("{:?}", entry.kind).to_lowercase();
    let mut a = vec![
        "--role".into(), role.into(),
        "--kind".into(), kind,
        "--transport".into(), transport.into(),
        "--endpoint".into(), endpoint.into(),
        "--payload-bytes".into(), entry.payload_bytes.to_string(),
        "--messages".into(), entry.messages.to_string(),
        "--warmup".into(), entry.warmup_messages.to_string(),
    ];
    if let Some(p) = entry.peers {
        a.push("--peers".into());
        a.push(p.to_string());
    }
    if let Some(v) = &entry.target.variant {
        a.push("--variant".into());
        a.push(v.clone());
    }
    for (k, val) in &entry.target.knobs {
        a.push("--knob".into());
        a.push(format!("{k}={val}"));
    }
    a
}

/// Endpoint for a cell. ipc gets a unique socket path (returned for cleanup);
/// tcp claims a free loopback port. netns is intentionally not used here: it
/// needs root and is pointless on a single-core dev host. The bare-metal path
/// will add netns isolation.
fn make_endpoint(entry: &MatrixEntry, cell_id: &str) -> anyhow::Result<(String, Option<PathBuf>)> {
    match entry.transport {
        Transport::Ipc => {
            let path = std::env::temp_dir().join(format!("zmq-arena-{cell_id}.sock"));
            let _ = std::fs::remove_file(&path);
            Ok((format!("ipc://{}", path.display()), Some(path)))
        }
        Transport::TcpNetns => {
            let l = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = l.local_addr()?.port();
            drop(l); // release so the target can rebind
            Ok((format!("tcp://127.0.0.1:{port}"), None))
        }
        Transport::Inproc => {
            anyhow::bail!("inproc needs a single in-process target, not a spawned pair")
        }
    }
}

/// Wait for a child up to `deadline`, killing it on timeout. Returns whether it
/// exited on its own.
fn wait_until(child: &mut Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
}

/// Like `wait_until`, but also samples the child's peak resident set size from
/// `/proc/<pid>/status` on each poll, returning the high-water mark in bytes
/// alongside the finished flag. This is the unprivileged peak-memory path: it
/// needs neither root nor a cgroup, so it works on any host. VmHWM is itself a
/// kernel high-water mark, so the last reading before the process exits is its
/// true peak; sampling the max across polls guards against a missed final read.
fn wait_until_peak(child: &mut Child, deadline: Instant) -> (bool, u64) {
    let pid = child.id();
    let mut peak = 0u64;
    loop {
        if let Some(rss) = crate::telemetry::peak_rss_bytes(pid) {
            peak = peak.max(rss);
        }
        match child.try_wait() {
            Ok(Some(_)) => return (true, peak),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (false, peak);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return (false, peak),
        }
    }
}

/// Best-effort cgroup leaf. Returns None (with a one-line warning) when
/// provisioning fails, e.g. when not root. The arena then runs without
/// isolation, which is fine for functional tests on a dev VM.
static CGROUP_WARNED: AtomicBool = AtomicBool::new(false);

fn try_cgroup(run_id: &str, leaf: &str, isolation: &Isolation) -> Option<Cgroup> {
    let cg = Cgroup::new(run_id, leaf, isolation.clone());
    if let Err(e) = cg.create().and_then(|_| cg.apply_limits()) {
        // Warn once per run, not once per leaf.
        if !CGROUP_WARNED.swap(true, Ordering::Relaxed) {
            eprintln!("  cgroups unavailable ({e}); running without isolation (run as root for pinning)");
        }
        return None;
    }
    Some(cg)
}

/// PUSH/PULL throughput: spawn consumer (binds) then producer (connects), time
/// the consumer's wall-clock receive of the whole block, compute msgs/s, and
/// capture CPU + context switches via getrusage deltas and peak memory via the
/// cgroup. Single-process latency timing is not used; throughput here folds the
/// warmup block into the timed window (functional-grade on a dev host).
fn run_throughput(
    run_id: &str,
    cell_id: &str,
    entry: &MatrixEntry,
    isolation: &Isolation,
) -> anyhow::Result<CellRecord> {
    let (endpoint, ipc_path) = make_endpoint(entry, cell_id)?;
    let binary = &entry.target.binary;

    let sub_cg = try_cgroup(run_id, &format!("{cell_id}-sub"), isolation);
    let pub_cg = try_cgroup(run_id, &format!("{cell_id}-pub"), isolation);

    let (cpu0, sched0) = crate::telemetry::rusage_children();

    let mut consumer = ProcCommand::new(binary)
        .args(target_args(entry, "sub", &endpoint))
        .spawn()
        .with_context(|| format!("spawning consumer {}", binary.display()))?;
    if let Some(cg) = &sub_cg {
        let _ = cg.attach(consumer.id());
    }
    std::thread::sleep(Duration::from_millis(150)); // let the consumer bind
    // Open the syscall probe after the bind settle, so the engine's io_threads
    // and runtime workers already exist and are enumerated. The measured receive
    // loop starts only once the producer connects, below, so it is fully covered.
    let syscall_probe = crate::telemetry::SyscallProbe::open(consumer.id());

    let total = entry.messages + entry.warmup_messages;
    let t0 = Instant::now();
    let mut producer = ProcCommand::new(binary)
        .args(target_args(entry, "pub", &endpoint))
        .spawn()
        .with_context(|| format!("spawning producer {}", binary.display()))?;
    if let Some(cg) = &pub_cg {
        let _ = cg.attach(producer.id());
    }

    let budget = Duration::from_secs((total / 50_000).max(10));
    let (consumer_ok, rss_peak) = wait_until_peak(&mut consumer, Instant::now() + budget);
    let elapsed = t0.elapsed();
    let syscalls = syscall_probe.read();
    let _ = wait_until(&mut producer, Instant::now() + Duration::from_secs(5));

    if let Some(p) = ipc_path {
        let _ = std::fs::remove_file(p);
    }
    if !consumer_ok {
        anyhow::bail!("cell timed out after {budget:?} (consumer did not finish)");
    }

    let secs = elapsed.as_secs_f64().max(1e-9);
    let msgs_per_s = total as f64 / secs;
    let mbps = msgs_per_s * entry.payload_bytes as f64 / 1e6;

    let (cpu1, sched1) = crate::telemetry::rusage_children();
    let sched = SchedCounters {
        voluntary_ctxt_switches: sched1
            .voluntary_ctxt_switches
            .saturating_sub(sched0.voluntary_ctxt_switches),
        involuntary_ctxt_switches: sched1
            .involuntary_ctxt_switches
            .saturating_sub(sched0.involuntary_ctxt_switches),
    };
    // Prefer the cgroup high-water mark (root, covers all procs in the leaf);
    // fall back to the measured process's VmHWM so peak memory is populated even
    // unprivileged.
    let peak_memory_bytes = sub_cg
        .as_ref()
        .and_then(|cg| cg.peak_memory_bytes().ok())
        .filter(|&v| v > 0)
        .unwrap_or(rss_peak);

    Ok(CellRecord {
        run_id: run_id.to_string(),
        cell_id: cell_id.to_string(),
        entry: entry.clone(),
        meta: TargetMeta::default(),
        // Latency is not measured for throughput cells; the render step emits
        // null for it. A zeroed snapshot keeps the record well-formed.
        latency: LatencySnapshot {
            count: 0, min_ns: 0, max_ns: 0, p50_ns: 0, p90_ns: 0, p99_ns: 0, p999_ns: 0,
        },
        throughput: Throughput { msgs_per_s, mbps },
        cpu_seconds: (cpu1 - cpu0).max(0.0),
        syscalls,
        sched,
        peak_memory_bytes,
    })
}

/// REQ/REP latency. Spawn the REP server (binds, echoes), then the REQ client
/// (connects), which times each round-trip and prints the quantiles. Read the
/// client's stdout, parse it, then kill the server. CPU and context switches
/// come from getrusage deltas; memory from the cgroup when available.
fn run_latency(
    run_id: &str,
    cell_id: &str,
    entry: &MatrixEntry,
    isolation: &Isolation,
) -> anyhow::Result<CellRecord> {
    let (endpoint, ipc_path) = make_endpoint(entry, cell_id)?;
    let binary = &entry.target.binary;

    let sub_cg = try_cgroup(run_id, &format!("{cell_id}-sub"), isolation);
    let pub_cg = try_cgroup(run_id, &format!("{cell_id}-pub"), isolation);

    let (cpu0, sched0) = crate::telemetry::rusage_children();

    let mut server = ProcCommand::new(binary)
        .args(target_args(entry, "sub", &endpoint))
        .spawn()
        .with_context(|| format!("spawning REP server {}", binary.display()))?;
    if let Some(cg) = &sub_cg {
        let _ = cg.attach(server.id());
    }
    std::thread::sleep(Duration::from_millis(150)); // let the server bind

    let mut client = ProcCommand::new(binary)
        .args(target_args(entry, "pub", &endpoint))
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning REQ client {}", binary.display()))?;
    if let Some(cg) = &pub_cg {
        let _ = cg.attach(client.id());
    }
    // Brief settle so the client's io_thread / runtime workers exist before the
    // probe enumerates threads. The probe then attaches part-way through the
    // discarded warmup and covers the whole measured round-trip loop.
    std::thread::sleep(Duration::from_millis(120));
    let syscall_probe = crate::telemetry::SyscallProbe::open(client.id());

    let budget = Duration::from_secs((entry.messages / 20_000).max(15));
    let (ok, rss_peak) = wait_until_peak(&mut client, Instant::now() + budget);
    let syscalls = syscall_probe.read();
    let mut out = String::new();
    if let Some(mut so) = client.stdout.take() {
        let _ = so.read_to_string(&mut out);
    }
    let _ = server.kill();
    let _ = server.wait();
    if let Some(p) = ipc_path {
        let _ = std::fs::remove_file(p);
    }
    if !ok {
        anyhow::bail!("latency cell timed out after {budget:?}");
    }

    let latency = match parse_latency(&out) {
        Some(l) => l,
        None => anyhow::bail!("no LATENCY line in client output: {out:?}"),
    };

    let (cpu1, sched1) = crate::telemetry::rusage_children();
    let sched = SchedCounters {
        voluntary_ctxt_switches: sched1
            .voluntary_ctxt_switches
            .saturating_sub(sched0.voluntary_ctxt_switches),
        involuntary_ctxt_switches: sched1
            .involuntary_ctxt_switches
            .saturating_sub(sched0.involuntary_ctxt_switches),
    };
    let peak_memory_bytes = pub_cg
        .as_ref()
        .and_then(|cg| cg.peak_memory_bytes().ok())
        .filter(|&v| v > 0)
        .unwrap_or(rss_peak);

    Ok(CellRecord {
        run_id: run_id.to_string(),
        cell_id: cell_id.to_string(),
        entry: entry.clone(),
        meta: TargetMeta::default(),
        latency,
        // Throughput is not measured for latency cells; render emits null.
        throughput: Throughput::default(),
        cpu_seconds: (cpu1 - cpu0).max(0.0),
        syscalls,
        sched,
        peak_memory_bytes,
    })
}

/// Duration-based multi-peer throughput: pubsub, fanout, fanin. One coordinator
/// binds and accepts `peers` workers; the measured consumer counts for the
/// window and prints `THROUGHPUT <count> <elapsed>`. For pubsub and fanout the
/// producer is the coordinator (`producer_binds = true`); for fanin the single
/// consumer (the sink) is the coordinator (`producer_binds = false`). msgs/s is
/// per measured consumer. TCP only.
fn run_multipeer(
    run_id: &str,
    cell_id: &str,
    entry: &MatrixEntry,
    isolation: &Isolation,
    producer_binds: bool,
) -> anyhow::Result<CellRecord> {
    if !matches!(entry.transport, Transport::TcpNetns) {
        anyhow::bail!("{:?} is supported on tcp only", entry.kind);
    }
    let peers = entry.peers.unwrap_or(1).max(1);
    let duration = entry.duration_secs.unwrap_or(2.0);
    let (endpoint, _ipc) = make_endpoint(entry, cell_id)?;
    let binary = &entry.target.binary;

    let pub_cg = try_cgroup(run_id, &format!("{cell_id}-pub"), isolation);
    let sub_cg = try_cgroup(run_id, &format!("{cell_id}-sub"), isolation);
    let (cpu0, sched0) = crate::telemetry::rusage_children();

    let mut others: Vec<Child> = Vec::new();
    let measured;

    if producer_binds {
        // pubsub / fanout: one producer binds and accepts `peers`; the consumers
        // connect, one measured and the rest draining.
        let prod = ProcCommand::new(binary)
            .args(peer_args(entry, "pub", &endpoint, true, duration))
            .spawn()
            .with_context(|| format!("spawning producer {}", binary.display()))?;
        if let Some(cg) = &pub_cg {
            let _ = cg.attach(prod.id());
        }
        others.push(prod);
        std::thread::sleep(Duration::from_millis(200));

        let m = ProcCommand::new(binary)
            .args(peer_args(entry, "sub", &endpoint, false, duration))
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning measured consumer {}", binary.display()))?;
        if let Some(cg) = &sub_cg {
            let _ = cg.attach(m.id());
        }
        for _ in 1..peers {
            let d = ProcCommand::new(binary)
                .args(peer_args(entry, "sub", &endpoint, false, duration))
                .stdout(Stdio::null())
                .stderr(Stdio::null()) // non-measured peers do not spam stderr
                .spawn()
                .with_context(|| format!("spawning drain consumer {}", binary.display()))?;
            if let Some(cg) = &sub_cg {
                let _ = cg.attach(d.id());
            }
            others.push(d);
        }
        measured = m;
    } else {
        // fanin: the single consumer (sink) binds and accepts `peers`; the
        // producers connect and send forever.
        let m = ProcCommand::new(binary)
            .args(peer_args(entry, "sub", &endpoint, true, duration))
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning measured sink {}", binary.display()))?;
        if let Some(cg) = &sub_cg {
            let _ = cg.attach(m.id());
        }
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..peers {
            let p = ProcCommand::new(binary)
                .args(peer_args(entry, "pub", &endpoint, false, duration))
                .stderr(Stdio::null()) // non-measured peers do not spam stderr
                .spawn()
                .with_context(|| format!("spawning producer {}", binary.display()))?;
            if let Some(cg) = &pub_cg {
                let _ = cg.attach(p.id());
            }
            others.push(p);
        }
        measured = m;
    }

    let mut measured = measured;
    // Settle so the measured consumer's io_threads / runtime workers exist before
    // the probe enumerates threads (the fan-in sink already settled above; the
    // pubsub/fanout consumer was just spawned). A short slice of the duration
    // window is uncounted as a result, acceptable for a characterization metric.
    std::thread::sleep(Duration::from_millis(150));
    let syscall_probe = crate::telemetry::SyscallProbe::open(measured.id());
    let budget = Duration::from_secs_f64(duration + 15.0);
    let (ok, rss_peak) = wait_until_peak(&mut measured, Instant::now() + budget);
    let mut out = String::new();
    if let Some(mut so) = measured.stdout.take() {
        let _ = so.read_to_string(&mut out);
    }
    let syscalls = syscall_probe.read();
    for mut c in others {
        let _ = c.kill();
        let _ = c.wait();
    }
    if !ok {
        anyhow::bail!("{:?} measured consumer timed out", entry.kind);
    }

    let (count, elapsed) = parse_throughput_line(&out)
        .ok_or_else(|| anyhow::anyhow!("no THROUGHPUT line from consumer: {out:?}"))?;
    let msgs_per_s = count as f64 / elapsed.max(1e-9);
    let mbps = msgs_per_s * entry.payload_bytes as f64 / 1e6;

    let (cpu1, sched1) = crate::telemetry::rusage_children();
    let sched = SchedCounters {
        voluntary_ctxt_switches: sched1
            .voluntary_ctxt_switches
            .saturating_sub(sched0.voluntary_ctxt_switches),
        involuntary_ctxt_switches: sched1
            .involuntary_ctxt_switches
            .saturating_sub(sched0.involuntary_ctxt_switches),
    };
    let peak_memory_bytes = sub_cg
        .as_ref()
        .and_then(|cg| cg.peak_memory_bytes().ok())
        .filter(|&v| v > 0)
        .unwrap_or(rss_peak);

    Ok(CellRecord {
        run_id: run_id.to_string(),
        cell_id: cell_id.to_string(),
        entry: entry.clone(),
        meta: TargetMeta::default(),
        latency: LatencySnapshot {
            count: 0, min_ns: 0, max_ns: 0, p50_ns: 0, p90_ns: 0, p99_ns: 0, p999_ns: 0,
        },
        throughput: Throughput { msgs_per_s, mbps },
        cpu_seconds: (cpu1 - cpu0).max(0.0),
        syscalls,
        sched,
        peak_memory_bytes,
    })
}
