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

use std::path::PathBuf;
use std::process::{Child, Command as ProcCommand};
use std::sync::atomic::{AtomicBool, Ordering};
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
    latency: LatencySnapshot,
    throughput: Throughput,
    cpu_seconds: f64,
    syscalls: SyscallCounters,
    sched: SchedCounters,
    peak_memory_bytes: u64,
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
    match entry.kind {
        Kind::Throughput => run_throughput(run_id, cell_id, entry, isolation),
        other => anyhow::bail!(
            "kind {other:?} not yet implemented in the runnable path (only throughput)"
        ),
    }
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
    let consumer_ok = wait_until(&mut consumer, Instant::now() + budget);
    let elapsed = t0.elapsed();
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
    let peak_memory_bytes = sub_cg
        .as_ref()
        .and_then(|cg| cg.peak_memory_bytes().ok())
        .unwrap_or(0);

    Ok(CellRecord {
        run_id: run_id.to_string(),
        cell_id: cell_id.to_string(),
        entry: entry.clone(),
        // Latency is not measured for throughput cells; the render step emits
        // null for it. A zeroed snapshot keeps the record well-formed.
        latency: LatencySnapshot {
            count: 0, min_ns: 0, max_ns: 0, p50_ns: 0, p90_ns: 0, p99_ns: 0, p999_ns: 0,
        },
        throughput: Throughput { msgs_per_s, mbps },
        cpu_seconds: (cpu1 - cpu0).max(0.0),
        // Syscall counts still require the eBPF/perf path.
        syscalls: SyscallCounters::default(),
        sched,
        peak_memory_bytes,
    })
}
