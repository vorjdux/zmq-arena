//! zmq-arena target wrapper: monocoque (monocoque-rs, ZMTP on io_uring/compio).
//!
//! Implements the throughput kind (PUSH/PULL) against the monocoque-rs `zmq`
//! API: the producer connects a PUSH socket and sends the whole block, the
//! consumer binds a PULL socket and receives it. The orchestrator times the
//! consumer externally. Other kinds bail and are skipped by the run loop.
//!
//! The runtime is compio (io_uring), so `main` runs under `#[compio::main]`. If
//! that attribute is unavailable in your compio version, wrap the body in
//! `compio::runtime::Runtime::new()?.block_on(async { ... })` instead.

use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};
use monocoque::zmq::{PullSocket, PushSocket};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "monocoque-target", version, about = "zmq-arena monocoque wrapper")]
struct Cli {
    #[arg(long, value_enum)]
    role: Role,
    #[arg(long, default_value = "throughput")]
    kind: String,
    /// Accepted; the endpoint already carries the transport scheme.
    #[arg(long)]
    transport: String,
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    payload_bytes: u32,
    #[arg(long)]
    messages: u64,
    #[arg(long, default_value_t = 0)]
    warmup: u64,
    #[arg(long)]
    peers: Option<u32>,
    #[arg(long, default_value = "default")]
    variant: String,
    /// Accepted and currently ignored; monocoque tuning knobs are not wired yet.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

#[compio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    eprintln!(
        "monocoque-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role, cli.kind, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, cli.variant
    );

    match cli.kind.as_str() {
        "throughput" => run_throughput(&cli).await,
        other => bail!("monocoque: kind '{other}' not implemented yet (only throughput)"),
    }
}

async fn run_throughput(cli: &Cli) -> Result<()> {
    let total = cli.messages + cli.warmup;
    let payload = vec![0u8; cli.payload_bytes as usize];

    match cli.role {
        Role::Pub => {
            let mut push = PushSocket::connect(&cli.endpoint).await?;
            for _ in 0..total {
                push.send(vec![payload.clone().into()]).await?;
            }
        }
        Role::Sub => {
            let mut pull = PullSocket::bind(&cli.endpoint).await?;
            for _ in 0..total {
                let _ = pull.recv().await?;
            }
        }
    }
    Ok(())
}
