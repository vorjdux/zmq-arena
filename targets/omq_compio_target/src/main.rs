//! zmq-arena target wrapper: omq-compio (io_uring, single-thread).
//!
//! Unified target CLI (see ../../README.md). omq-compio exposes the same public
//! Socket API as omq-tokio (the two backends are kept at parity upstream), so
//! the socket loop mirrors the omq-tokio wrapper; only the runtime entry differs
//! (compio drives io_uring, not tokio). The dependency is wired in; the loop is
//! left to the maintainer, who also chooses the compio runtime entry point.

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
#[command(name = "omq-compio-target", version, about = "zmq-arena omq-compio wrapper")]
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
    /// Runtime variant selector. omq-compio exposes "default" and "single_thread"
    /// (the ST runtime); the wrapper maps this to the compio runtime flavor.
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
/// engine version is read from Cargo.lock at build time (see build.rs). omq-compio
/// runs on the compio io_uring runtime; describe stays binary-level until the
/// socket loop lands.
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"omq\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"multi\",\"io\":\"io_uring\"}}"
        ),
        env!("ENGINE_VERSION")
    )
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("describe") {
        println!("{}", describe());
        return Ok(());
    }
    let cli = Cli::parse();
    let knobs: BTreeMap<String, String> = cli.knobs.iter().cloned().collect();

    eprintln!(
        "omq-compio-target: role={:?} pattern={:?} transport={:?} endpoint={} payload={}B msgs={} warmup={} knobs={:?}",
        cli.role, cli.pattern, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, knobs
    );

    // TODO(maintainer): enter the compio runtime, then dispatch on cli.role to
    // the Push/Pub producer or Pull/Sub consumer using the same omq Socket API
    // documented in the omq-tokio wrapper.
    match cli.role {
        Role::Pub => bail!("omq-compio publisher loop not implemented"),
        Role::Sub => bail!("omq-compio subscriber loop not implemented"),
    }
}
