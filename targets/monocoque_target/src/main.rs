//! zmq-arena target wrapper: monocoque (monocoque-rs, ZMTP on io_uring/compio).
//!
//! Implements the throughput kind (PUSH/PULL) over TCP and IPC against
//! monocoque-rs. The producer connects a PUSH socket and sends the whole block;
//! the consumer binds a listener, accepts one connection, wraps it as a PULL
//! socket, and drains the block. The orchestrator times the consumer
//! externally. Other kinds bail and are skipped by the run loop.
//!
//! monocoque runs on the compio io_uring runtime, driven via
//! `compio::runtime::Runtime::block_on`.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use clap::{Parser, ValueEnum};
use compio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use monocoque::zmq::{PullSocket, PushSocket};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

enum Transport {
    Tcp(SocketAddr),
    Ipc(PathBuf),
}

fn parse_endpoint(ep: &str) -> Result<Transport> {
    if let Some(a) = ep.strip_prefix("tcp://") {
        Ok(Transport::Tcp(
            a.parse().with_context(|| format!("parsing tcp address {a}"))?,
        ))
    } else if let Some(p) = ep.strip_prefix("ipc://") {
        Ok(Transport::Ipc(PathBuf::from(p)))
    } else {
        bail!("unsupported endpoint (need tcp:// or ipc://): {ep}")
    }
}

#[derive(Parser, Debug)]
#[command(name = "monocoque-target", version, about = "zmq-arena monocoque wrapper")]
struct Cli {
    #[arg(long, value_enum)]
    role: Role,
    #[arg(long, default_value = "throughput")]
    kind: String,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    eprintln!(
        "monocoque-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role, cli.kind, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, cli.variant
    );

    if cli.kind != "throughput" {
        bail!("monocoque: kind '{}' not implemented yet (only throughput)", cli.kind);
    }
    let transport = parse_endpoint(&cli.endpoint)?;
    let total = cli.messages + cli.warmup;
    let payload = Bytes::from(vec![0u8; cli.payload_bytes as usize]);
    let role = cli.role;

    compio::runtime::Runtime::new()?.block_on(async move {
        match (role, transport) {
            (Role::Sub, Transport::Tcp(addr)) => {
                let listener = TcpListener::bind(addr).await?;
                let (stream, _) = listener.accept().await?;
                let mut pull = PullSocket::from_tcp(stream).await?;
                let mut got: u64 = 0;
                while got < total {
                    match pull.recv().await? {
                        Some(_) => got += 1,
                        None => break,
                    }
                }
            }
            (Role::Pub, Transport::Tcp(addr)) => {
                let mut push = PushSocket::<TcpStream>::connect(addr).await?;
                for _ in 0..total {
                    push.send(vec![payload.clone()]).await?;
                }
            }
            (Role::Sub, Transport::Ipc(path)) => {
                let listener = UnixListener::bind(&path).await?;
                let (stream, _) = listener.accept().await?;
                let mut pull = PullSocket::from_unix_stream(stream).await?;
                let mut got: u64 = 0;
                while got < total {
                    match pull.recv().await? {
                        Some(_) => got += 1,
                        None => break,
                    }
                }
            }
            (Role::Pub, Transport::Ipc(path)) => {
                let stream = UnixStream::connect(&path).await?;
                let mut push = PushSocket::from_unix_stream(stream).await?;
                for _ in 0..total {
                    push.send(vec![payload.clone()]).await?;
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}
