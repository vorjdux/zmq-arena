//! zmq-arena target wrapper: celerity (sans-IO ZMTP 3.1 + Tokio).
//!
//! Unified target CLI (see ../../README.md). celerity is wired in. The socket
//! loop is left to the maintainer: drive the sans-IO `CelerityPeer` core through
//! the Tokio transport in `celerity::io`. Verify the exact API against the 0.2.x
//! docs before implementing.
//!
//! Endpoint note: celerity's own CLI accepts bare `host:port` for TCP and
//! `ipc:///path` for IPC. For remote (non-loopback) TCP it defaults to failing
//! closed unless CURVE-RS is configured, so loopback-in-netns cells must use a
//! loopback address or an explicit insecure opt-in.

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
#[command(name = "celerity-target", version, about = "zmq-arena celerity wrapper")]
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

/// One-line JSON classification the orchestrator captures into each record. The
/// engine version is read from Cargo.lock at build time (see build.rs). celerity
/// is a sans-IO ZMTP core driven by a tokio transport; describe stays binary-level
/// until the socket loop lands.
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"celerity\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"multi\",\"io\":\"epoll\"}}"
        ),
        env!("ENGINE_VERSION")
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("describe") {
        println!("{}", describe());
        return Ok(());
    }
    let cli = Cli::parse();
    let knobs: BTreeMap<String, String> = cli.knobs.iter().cloned().collect();

    eprintln!(
        "celerity-target: role={:?} pattern={:?} transport={:?} endpoint={} payload={}B msgs={} warmup={} knobs={:?}",
        cli.role, cli.pattern, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, knobs
    );

    match cli.role {
        Role::Pub => run_publisher(&cli, &knobs).await,
        Role::Sub => run_subscriber(&cli, &knobs).await,
    }
}

/// TODO(maintainer): celerity producer (PUSH/PUB via CelerityPeer + Tokio io).
async fn run_publisher(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("celerity publisher loop not implemented");
}

/// TODO(maintainer): celerity consumer (PULL/SUB), receive exactly messages.
async fn run_subscriber(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("celerity subscriber loop not implemented");
}
