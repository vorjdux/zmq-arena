//! zmq-arena target wrapper: zmq.rs (the `zeromq` crate, pure-Rust ZeroMQ on Tokio).
//!
//! All five kinds over the trait-based `zeromq` 0.6 API. The crate's sockets take
//! a full endpoint string ("tcp://host:port", "ipc:///path") and manage peer
//! accept/connect internally, so one bind fair-queues (PULL) or load-balances
//! (PUSH) across N connected peers; there is no manual accept loop as in the
//! lower-level engines.
//!
//! Role and bind contract, set by the orchestrator (see targets/README.md):
//!   throughput  PULL(sub) binds,  PUSH(pub) connects
//!   latency     REP(sub) binds,   REQ(pub) connects
//!   pubsub      PUB(pub) binds,   SUB(sub) connect    (--bind on pub)
//!
//! Fan-out and fan-in are not supported on this engine. zmq.rs 0.6 only carries
//! the bound PULL / connecting PUSH direction and one-to-many PUB/SUB; a bound
//! PUSH does not round-robin to connecting PULL peers, and a bound PULL does not
//! fair-queue across more than one connected PUSH. Both were verified to hang
//! against this harness's fan-out (1 PUSH : N PULL) and fan-in (N PUSH : 1 PULL)
//! topology, so the wrapper rejects those kinds up front rather than block until
//! the orchestrator's budget kills it. Fabricating a number from a degraded path
//! would be worse than a recorded skip.
//!
//! Duration-based kinds are TCP only, matching the orchestrator, which rejects
//! multi-peer cells on non-TCP transports.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};
use zeromq::prelude::*;
use zeromq::{PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket, SubSocket, ZmqMessage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "zeromq-rs-target", version, about = "zmq-arena zmq.rs wrapper")]
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
    /// Accepted and ignored; zmq.rs tuning knobs are not wired yet.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record. The
/// engine version is read from Cargo.lock at build time (see build.rs).
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"zmq.rs\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"multi\",\"io\":\"epoll\"}}"
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
    eprintln!(
        "zeromq-rs-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role, cli.kind, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, cli.variant
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let payload = ZmqMessage::from(vec![b'x'; cli.payload_bytes as usize]);
    let ep = cli.endpoint.as_str();
    let role = cli.role;
    let duration = Duration::from_secs_f64(cli.duration_secs);
    match cli.kind.as_str() {
        "throughput" => run_throughput(role, ep, cli.messages, cli.warmup, &payload).await,
        "latency" => run_latency(role, ep, cli.messages, cli.warmup, &payload).await,
        "pubsub" => run_pubsub(role, ep, duration, &payload).await,
        "fanout" => bail!(
            "zmq.rs (zeromq 0.6) does not support fan-out in this harness: a bound \
             PUSH socket does not round-robin to connecting PULL peers. Supported \
             kinds: throughput, latency, pubsub."
        ),
        "fanin" => bail!(
            "zmq.rs (zeromq 0.6) does not support fan-in in this harness: a bound \
             PULL socket does not fair-queue across multiple connecting PUSH peers. \
             Supported kinds: throughput, latency, pubsub."
        ),
        other => bail!("zmq.rs: kind '{other}' not implemented"),
    }
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

/// PUSH connects and sends `messages + warmup` messages; PULL binds, drains the
/// `warmup` prefix untimed, then times only the `messages` steady-state block and
/// prints `THROUGHPUT <messages> <elapsed_secs>`. Timing the measured block inside
/// the target (not the orchestrator's wall clock) keeps process spawn, the
/// connection handshake, and the warmup transfer out of the rate. zmq.rs applies
/// its own back-pressure, so the send loop blocks on the socket HWM rather than
/// dropping.
async fn run_throughput(
    role: Role,
    ep: &str,
    messages: u64,
    warmup: u64,
    payload: &ZmqMessage,
) -> Result<()> {
    match role {
        Role::Sub => {
            let mut pull = PullSocket::new();
            pull.bind(ep).await?;
            for _ in 0..warmup {
                pull.recv().await?;
            }
            // Warmup drained; start the clock and time only the steady-state block.
            let t0 = Instant::now();
            for _ in 0..messages {
                pull.recv().await?;
            }
            let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
            println!("THROUGHPUT {messages} {elapsed:.6}");
        }
        Role::Pub => {
            let mut push = PushSocket::new();
            push.connect(ep).await?;
            for _ in 0..(messages + warmup) {
                push.send(payload.clone()).await?;
            }
        }
    }
    Ok(())
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

/// REP binds and echoes until the peer disconnects. REQ connects, runs `warmup`
/// untimed round-trips, then times `messages` and prints the quantiles the
/// orchestrator parses.
async fn run_latency(
    role: Role,
    ep: &str,
    messages: u64,
    warmup: u64,
    payload: &ZmqMessage,
) -> Result<()> {
    match role {
        Role::Sub => {
            let mut rep = RepSocket::new();
            rep.bind(ep).await?;
            loop {
                let msg = match rep.recv().await {
                    Ok(m) => m,
                    Err(_) => break, // client gone
                };
                if rep.send(msg).await.is_err() {
                    break;
                }
            }
        }
        Role::Pub => {
            let mut req = ReqSocket::new();
            req.connect(ep).await?;
            for _ in 0..warmup {
                req.send(payload.clone()).await?;
                req.recv().await?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                req.send(payload.clone()).await?;
                req.recv().await?;
                rtts.push(t.elapsed().as_nanos() as u64);
            }
            print_latency(&mut rtts);
        }
    }
    Ok(())
}

fn print_latency(rtts: &mut [u64]) {
    if rtts.is_empty() {
        println!("LATENCY 0 0 0 0 0 0 0");
        return;
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
}

// ── pub/sub (PUB/SUB) ───────────────────────────────────────────────────────

/// PUB binds and broadcasts forever (killed by the orchestrator). SUB connects,
/// subscribes to everything, and counts for the window. TCP only.
async fn run_pubsub(role: Role, ep: &str, duration: Duration, payload: &ZmqMessage) -> Result<()> {
    require_tcp("pubsub", ep)?;
    match role {
        Role::Pub => {
            let mut publisher = PubSocket::new();
            publisher.bind(ep).await?;
            loop {
                if publisher.send(payload.clone()).await.is_err() {
                    break;
                }
            }
        }
        Role::Sub => {
            let mut sub = SubSocket::new();
            sub.connect(ep).await?;
            sub.subscribe("").await?;
            count_window(&mut sub, duration).await;
        }
    }
    Ok(())
}

// ── shared helpers ──────────────────────────────────────────────────────────

/// Count received messages over `duration`, starting the clock on the first
/// message to skip the connect/accept ramp, then print `THROUGHPUT count secs`.
async fn count_window<S: SocketRecv>(sock: &mut S, duration: Duration) {
    if sock.recv().await.is_err() {
        println!("THROUGHPUT 0 0.000001");
        return;
    }
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        match sock.recv().await {
            Ok(_) => count += 1,
            Err(_) => break, // producer gone
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    println!("THROUGHPUT {count} {elapsed:.6}");
}

fn require_tcp(kind: &str, ep: &str) -> Result<()> {
    if !ep.starts_with("tcp://") {
        bail!("zmq.rs {kind}: tcp only (got {ep})");
    }
    Ok(())
}
