//! zmq-arena target wrapper: rzmq (io_uring + TCP_CORK).
//!
//! Unified target CLI (see ../../README.md). The rzmq dependency is wired in
//! with io-uring + curve features. The socket loop is left to the maintainer;
//! verify rzmq's socket/context API against the pinned 0.5.x docs before
//! implementing (do not assume method names here).
//!
//! IPC note: rzmq uses pathname IPC (`ipc:///tmp/...`), not abstract-namespace
//! sockets. The matrix endpoint must reflect that for ipc cells.

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
#[command(name = "rzmq-target", version, about = "zmq-arena rzmq wrapper")]
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let knobs: BTreeMap<String, String> = cli.knobs.iter().cloned().collect();

    eprintln!(
        "rzmq-target: role={:?} pattern={:?} transport={:?} endpoint={} payload={}B msgs={} warmup={} knobs={:?}",
        cli.role, cli.pattern, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, knobs
    );

    match cli.role {
        Role::Pub => run_publisher(&cli, &knobs).await,
        Role::Sub => run_subscriber(&cli, &knobs).await,
    }
}

/// TODO(maintainer): rzmq PUSH/PUB producer. Apply knobs the engine exposes
/// (HWM, io_uring SQ depth), then send warmup + messages payloads.
async fn run_publisher(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("rzmq publisher loop not implemented");
}

/// TODO(maintainer): rzmq PULL/SUB consumer. Receive exactly messages payloads
/// after warmup, no dropping.
async fn run_subscriber(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("rzmq subscriber loop not implemented");
}
