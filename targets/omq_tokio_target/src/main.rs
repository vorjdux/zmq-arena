//! zmq-arena target wrapper: omq-tokio.
//!
//! Unified target CLI (see ../../README.md). The omq dependency is wired in;
//! the socket loop is left to the maintainer. The documented omq Socket API is
//! reproduced in the TODO comments as a starting point, but the exact SocketType
//! variants, Options fields, and payload construction must be verified against
//! the pinned omq revision before relying on them.

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
#[command(name = "omq-tokio-target", version, about = "zmq-arena omq-tokio wrapper")]
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
    /// Runtime variant selector. omq-tokio exposes "default" (current-thread)
    /// and "multi_thread"; the wrapper maps this to the tokio runtime flavor.
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
/// engine version is read from Cargo.lock at build time (see build.rs). omq-tokio
/// default is the current-thread runtime; the multi_thread variant is a separate
/// series in the matrix. describe stays binary-level until the socket loop lands.
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"omq\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"single\",\"io\":\"epoll\"}}"
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
        "omq-tokio-target: role={:?} pattern={:?} transport={:?} endpoint={} payload={}B msgs={} warmup={} knobs={:?}",
        cli.role, cli.pattern, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, knobs
    );

    match cli.role {
        Role::Pub => run_publisher(&cli, &knobs).await,
        Role::Sub => run_subscriber(&cli, &knobs).await,
    }
}

/// TODO(maintainer): producer end. Documented omq-tokio API:
///   use omq_tokio::{Message, Options, Socket, SocketType};
///   let s = Socket::new(SocketType::Push /* or Pub */, Options::default());
///   s.connect(cli.endpoint.parse()?).await?;
///   for _ in 0..(cli.warmup + cli.messages) { s.send(Message::single(payload)).await?; }
async fn run_publisher(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("omq-tokio publisher loop not implemented");
}

/// TODO(maintainer): consumer end.
///   let s = Socket::new(SocketType::Pull /* or Sub */, Options::default());
///   s.bind(cli.endpoint.parse()?).await?;
///   for _ in 0..(cli.warmup + cli.messages) { let _m = s.recv().await?; }
async fn run_subscriber(_cli: &Cli, _knobs: &BTreeMap<String, String>) -> Result<()> {
    bail!("omq-tokio subscriber loop not implemented");
}
