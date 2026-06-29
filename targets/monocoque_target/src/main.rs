//! zmq-arena target wrapper: monocoque (io_uring + thread-per-core).
//!
//! Parses the unified target CLI (see ../../README.md) and dispatches to the
//! publisher or subscriber role. The socket loop is left to the maintainer; the
//! CLI contract, knob parsing, and role dispatch are complete so the harness
//! can spawn this binary and the maintainer fills in exactly one function per
//! role.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Pattern {
    PubSub,
    PushPull,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Transport {
    Tcp,
    Ipc,
}

#[derive(Parser, Debug)]
#[command(name = "monocoque-target", version, about = "zmq-arena monocoque wrapper")]
struct Cli {
    #[arg(long, value_enum)]
    role: Role,
    #[arg(long, value_enum)]
    pattern: Pattern,
    #[arg(long, value_enum)]
    transport: Transport,
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    payload_bytes: u32,
    #[arg(long)]
    messages: u64,
    #[arg(long, default_value_t = 0)]
    warmup: u64,
    /// Repeatable maintainer-owned tuning, key=value.
    /// Benchmark kind: throughput | latency | pubsub | fanout | fanin.
    #[arg(long, default_value = "throughput")]
    kind: String,
    /// Subscriber/pusher count (pubsub/fanout/fanin); omitted otherwise.
    #[arg(long)]
    peers: Option<u32>,
    /// Runtime variant selector (engine-specific, e.g. "multi_thread").
    #[arg(long, default_value = "default")]
    variant: String,
    #[arg(long = "knob", value_parser = parse_knob)]
    knobs: Vec<(String, String)>,
}

fn parse_knob(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) => Ok((k.trim().to_string(), v.trim().to_string())),
        None => Err(format!("knob must be key=value, got `{s}`")),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let knobs: BTreeMap<String, String> = cli.knobs.iter().cloned().collect();

    eprintln!(
        "monocoque-target: role={:?} pattern={:?} transport={:?} endpoint={} payload={}B msgs={} warmup={} knobs={:?}",
        cli.role, cli.pattern, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, knobs
    );

    match cli.role {
        Role::Pub => run_publisher(&cli, &knobs),
        Role::Sub => run_subscriber(&cli, &knobs),
    }
}

/// TODO(maintainer): open the monocoque PUSH/PUB socket on `cli.endpoint`,
/// apply known knobs (`sq_depth`, `batch_size`, `sndhwm`), send `warmup` then
/// `messages` payloads of `payload_bytes`. No data dropping on push_pull.
fn run_publisher(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("monocoque publisher loop not implemented");
}

/// TODO(maintainer): open the monocoque PULL/SUB socket, drain `warmup` then
/// receive exactly `messages` payloads. The harness measures latency at the
/// boundary; the wrapper must not elide the round-trip.
fn run_subscriber(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("monocoque subscriber loop not implemented");
}
