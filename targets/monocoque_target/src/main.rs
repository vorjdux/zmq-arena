//! zmq-arena target wrapper: monocoque (monocoque-rs, ZMTP on io_uring/compio).
//!
//! Throughput (PUSH/PULL) and latency (REQ/REP) over TCP and IPC, written to
//! match monocoque's own bench peer:
//!   - the PUSH side enables write coalescing and flushes every 64 sends (plus a
//!     final flush for the last partial batch), which is monocoque's main
//!     throughput lever;
//!   - the PULL side drains batches with recv_into / try_recv_into into one
//!     reused buffer, allocating nothing per message;
//!   - REQ times each round-trip and prints the quantiles the orchestrator
//!     parses; REP echoes.
//!
//! The orchestrator spawns the consumer (binds) first, then the producer
//! (connects). monocoque runs on the compio io_uring runtime.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use bytes::Bytes;
use clap::{Parser, ValueEnum};
use compio::net::{TcpListener, UnixListener, UnixStream};
use monocoque::zmq::{PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket, SubSocket};
use monocoque::SocketOptions;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

enum Endpoint {
    Tcp(SocketAddr),
    Ipc(PathBuf),
}

fn parse_endpoint(ep: &str) -> Result<Endpoint> {
    if let Some(a) = ep.strip_prefix("tcp://") {
        Ok(Endpoint::Tcp(a.parse().map_err(|e| {
            anyhow::anyhow!("parsing tcp address {a}: {e}")
        })?))
    } else if let Some(p) = ep.strip_prefix("ipc://") {
        Ok(Endpoint::Ipc(PathBuf::from(p)))
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
    /// Present on the binding side of a multi-peer kind.
    #[arg(long)]
    bind: bool,
    /// Measurement window for duration-based kinds (pubsub/fanout/fanin).
    #[arg(long, default_value_t = 0.0)]
    duration_secs: f64,
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

    let ep = parse_endpoint(&cli.endpoint)?;
    let payload = Bytes::from(vec![b'x'; cli.payload_bytes as usize]);
    let role = cli.role;
    let kind = cli.kind.clone();
    let (messages, warmup) = (cli.messages, cli.warmup);
    let peers = cli.peers.unwrap_or(1).max(1);
    let duration = Duration::from_secs_f64(cli.duration_secs);

    compio::runtime::Runtime::new()?.block_on(async move {
        match kind.as_str() {
            "throughput" => run_throughput(role, ep, messages + warmup, &payload).await,
            "latency" => run_latency(role, ep, messages, warmup, &payload).await,
            "pubsub" => run_pubsub(role, ep, peers, duration, &payload).await,
            other => bail!("monocoque: kind '{other}' not implemented (throughput, latency, pubsub)"),
        }
    })?;
    Ok(())
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

async fn run_throughput(role: Role, ep: Endpoint, total: u64, payload: &Bytes) -> Result<()> {
    let coalesce = SocketOptions::default().with_write_coalescing(true);
    match (role, ep) {
        (Role::Pub, Endpoint::Tcp(addr)) => {
            let mut push = PushSocket::connect_with_options(addr, coalesce).await?;
            send_block(&mut push, total, payload).await?;
        }
        (Role::Pub, Endpoint::Ipc(path)) => {
            let stream = UnixStream::connect(&path).await?;
            let mut push = PushSocket::from_unix_stream_with_options(stream, coalesce).await?;
            send_block(&mut push, total, payload).await?;
        }
        (Role::Sub, Endpoint::Tcp(addr)) => {
            let listener = TcpListener::bind(addr).await?;
            let (stream, _) = listener.accept().await?;
            let mut pull = PullSocket::from_tcp(stream).await?;
            recv_block(&mut pull, total).await?;
        }
        (Role::Sub, Endpoint::Ipc(path)) => {
            let listener = UnixListener::bind(&path).await?;
            let (stream, _) = listener.accept().await?;
            let mut pull = PullSocket::from_unix_stream(stream).await?;
            recv_block(&mut pull, total).await?;
        }
    }
    Ok(())
}

async fn send_block<S>(push: &mut PushSocket<S>, total: u64, payload: &Bytes) -> Result<()>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    let mut i = 0u64;
    while i < total {
        push.send(vec![payload.clone()]).await?;
        i += 1;
        if i % 64 == 0 {
            push.flush().await?;
        }
    }
    push.flush().await?; // flush the last partial batch
    Ok(())
}

async fn recv_block<S>(pull: &mut PullSocket<S>, total: u64) -> Result<()>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    let mut buf: Vec<Bytes> = Vec::with_capacity(4);
    let mut count = 0u64;
    while count < total {
        match pull.recv_into(&mut buf).await? {
            true => {
                count += 1;
                while count < total {
                    match pull.try_recv_into(&mut buf)? {
                        true => count += 1,
                        false => break,
                    }
                }
            }
            false => break, // connection closed
        }
    }
    Ok(())
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

async fn run_latency(
    role: Role,
    ep: Endpoint,
    messages: u64,
    warmup: u64,
    payload: &Bytes,
) -> Result<()> {
    match (role, ep) {
        (Role::Sub, Endpoint::Tcp(addr)) => {
            let listener = TcpListener::bind(addr).await?;
            let (stream, _) = listener.accept().await?;
            let mut rep = RepSocket::from_tcp(stream).await?;
            echo_loop(&mut rep).await?;
        }
        (Role::Sub, Endpoint::Ipc(path)) => {
            let listener = UnixListener::bind(&path).await?;
            let (stream, _) = listener.accept().await?;
            let mut rep = RepSocket::from_unix_stream(stream).await?;
            echo_loop(&mut rep).await?;
        }
        (Role::Pub, Endpoint::Tcp(addr)) => {
            // ReqSocket::connect takes a host:port string, not a SocketAddr.
            let mut req = ReqSocket::connect(&addr.to_string()).await?;
            req_measure(&mut req, messages, warmup, payload).await?;
        }
        (Role::Pub, Endpoint::Ipc(path)) => {
            let stream = UnixStream::connect(&path).await?;
            let mut req = ReqSocket::from_unix_stream(stream).await?;
            req_measure(&mut req, messages, warmup, payload).await?;
        }
    }
    Ok(())
}

async fn echo_loop<S>(rep: &mut RepSocket<S>) -> Result<()>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    while let Some(msg) = rep.recv().await? {
        rep.send(msg).await?;
    }
    Ok(())
}

async fn req_measure<S>(
    req: &mut ReqSocket<S>,
    messages: u64,
    warmup: u64,
    payload: &Bytes,
) -> Result<()>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    for _ in 0..warmup {
        req.send(vec![payload.clone()]).await?;
        req.recv().await?;
    }
    let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
    for _ in 0..messages {
        let t = Instant::now();
        req.send(vec![payload.clone()]).await?;
        req.recv().await?;
        rtts.push(t.elapsed().as_nanos() as u64);
    }
    if rtts.is_empty() {
        println!("LATENCY 0 0 0 0 0 0 0");
        return Ok(());
    }
    rtts.sort_unstable();
    let q = |p: f64| -> u64 {
        let idx = ((rtts.len() as f64 * p) as usize).min(rtts.len() - 1);
        rtts[idx]
    };
    println!(
        "LATENCY {} {} {} {} {} {} {}",
        rtts.len(),
        rtts[0],
        q(0.50),
        q(0.90),
        q(0.99),
        q(0.999),
        rtts[rtts.len() - 1]
    );
    Ok(())
}

// ── pub/sub (PUB/SUB) ───────────────────────────────────────────────────────

/// PUB binds, accepts `peers` subscribers, then broadcasts forever (killed by
/// the orchestrator). SUB connects, subscribes, and counts for `duration`,
/// starting the timer on the first message to skip the accept ramp, then prints
/// `THROUGHPUT <count> <elapsed>`. TCP only for now.
async fn run_pubsub(
    role: Role,
    ep: Endpoint,
    peers: u32,
    duration: Duration,
    payload: &Bytes,
) -> Result<()> {
    let addr = match ep {
        Endpoint::Tcp(a) => a,
        Endpoint::Ipc(_) => bail!("monocoque pubsub: tcp only for now"),
    };
    match role {
        Role::Pub => {
            let mut publisher = PubSocket::bind(&addr.to_string()).await?;
            for _ in 0..peers {
                publisher.accept_subscriber().await?;
            }
            loop {
                let _ = publisher.send(vec![payload.clone()]).await;
            }
        }
        Role::Sub => {
            let mut sub = SubSocket::connect(&addr.to_string()).await?;
            sub.subscribe(b"").await?;
            let mut count: u64 = match sub.recv().await {
                Ok(Some(_)) => 1,
                _ => {
                    println!("THROUGHPUT 0 0.000001");
                    return Ok(());
                }
            };
            let t0 = Instant::now();
            let deadline = t0 + duration;
            while Instant::now() < deadline {
                match sub.recv().await {
                    Ok(Some(_)) => count += 1,
                    _ => break,
                }
            }
            let elapsed = t0.elapsed().as_secs_f64();
            println!("THROUGHPUT {count} {elapsed:.6}");
        }
    }
    Ok(())
}
