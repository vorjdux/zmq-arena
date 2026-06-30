//! zmq-arena target wrapper: omq-compio (paddor/omq.rs, ZMTP on compio/io_uring).
//!
//! Same omq `Socket` API as the tokio backend, driven by the compio io_uring
//! runtime. `build_default_runtime` sets up the proactor and the provided-buffer
//! pool omq uses for multishot recv. compio is single-threaded, so there is one
//! variant. Needs Linux 6.0+ at runtime.
//!
//! Role and bind contract, set by the orchestrator (see targets/README.md):
//!   throughput  PULL(sub) binds,  PUSH(pub) connects
//!   latency     REP(sub) binds,   REQ(pub) connects
//!   pubsub      PUB(pub) binds,   SUB(sub) connect    (--bind on pub)
//!   fanout      PUSH(pub) binds,  PULL(sub) connect   (--bind on pub)
//!   fanin       PULL(sub) binds,  PUSH(pub) connect   (--bind on sub)

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use clap::{Parser, ValueEnum};
use omq_compio::{Endpoint, Message, OnMute, Options, Socket, SocketType};

/// PUB options: block when a subscriber is full rather than spinning, matching
/// omq's own bench peer.
fn sender_opts() -> Options {
    Options::default().on_mute(OnMute::Block)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "omq-compio-target", version, about = "zmq-arena omq-compio wrapper")]
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
    /// Accepted and ignored; omq tuning knobs are not wired yet.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record. The
/// engine version comes from Cargo.lock at build time (see build.rs). compio is a
/// single-threaded io_uring runtime, so there is one variant.
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"omq\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"single\",\"io\":\"io_uring\"}}"
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
        "omq-compio-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role, cli.kind, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, cli.variant
    );

    let rt = omq_compio::build_default_runtime().map_err(|e| anyhow!("compio runtime: {e}"))?;
    rt.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let payload = Bytes::from(vec![b'x'; cli.payload_bytes as usize]);
    let ep: Endpoint = cli
        .endpoint
        .parse()
        .map_err(|_| anyhow!("invalid endpoint: {}", cli.endpoint))?;
    let total = cli.messages + cli.warmup;
    let duration = Duration::from_secs_f64(cli.duration_secs);
    match cli.kind.as_str() {
        "throughput" => run_throughput(cli.role, ep, total, payload).await,
        "latency" => run_latency(cli.role, ep, cli.messages, cli.warmup, payload).await,
        "pubsub" => run_pubsub(cli.role, ep, duration, payload).await,
        "fanout" => run_fanout(cli.role, ep, duration, payload).await,
        "fanin" => run_fanin(cli.role, ep, duration, payload).await,
        other => bail!("omq-compio: kind '{other}' not implemented"),
    }
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

async fn run_throughput(role: Role, ep: Endpoint, total: u64, payload: Bytes) -> Result<()> {
    match role {
        Role::Sub => {
            let pull = Socket::new(SocketType::Pull, Options::default());
            pull.bind(ep).await?;
            let mut count = 0u64;
            while count < total {
                pull.recv().await?;
                count += 1;
            }
        }
        Role::Pub => {
            let push = Socket::new(SocketType::Push, Options::default());
            push.connect(ep).await?;
            // Send until the consumer has its `total` and the orchestrator kills
            // this producer; omq queues into the socket actor, so exiting after a
            // fixed count can drop messages still in flight.
            loop {
                if push.send(Message::single(payload.clone())).await.is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

async fn run_latency(role: Role, ep: Endpoint, messages: u64, warmup: u64, payload: Bytes) -> Result<()> {
    match role {
        Role::Sub => {
            let rep = Socket::new(SocketType::Rep, Options::default());
            rep.bind(ep).await?;
            loop {
                match rep.recv().await {
                    Ok(msg) => {
                        if rep.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        Role::Pub => {
            let req = Socket::new(SocketType::Req, Options::default());
            req.connect(ep).await?;
            for _ in 0..warmup {
                req.send(Message::single(payload.clone())).await?;
                req.recv().await?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                req.send(Message::single(payload.clone())).await?;
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

async fn run_pubsub(role: Role, ep: Endpoint, duration: Duration, payload: Bytes) -> Result<()> {
    match role {
        Role::Pub => {
            let publisher = Socket::new(SocketType::Pub, sender_opts());
            publisher.bind(ep).await?;
            loop {
                if publisher.send(Message::single(payload.clone())).await.is_err() {
                    break;
                }
            }
        }
        Role::Sub => {
            let sub = Socket::new(SocketType::Sub, Options::default());
            sub.connect(ep).await?;
            sub.subscribe(Bytes::new()).await?;
            count_window(&sub, duration).await;
        }
    }
    Ok(())
}

// ── fan-out (1 PUSH -> N PULL) ───────────────────────────────────────────────

async fn run_fanout(role: Role, ep: Endpoint, duration: Duration, payload: Bytes) -> Result<()> {
    match role {
        Role::Pub => {
            let push = Socket::new(SocketType::Push, Options::default());
            push.bind(ep).await?;
            loop {
                if push.send(Message::single(payload.clone())).await.is_err() {
                    break;
                }
            }
        }
        Role::Sub => {
            let pull = Socket::new(SocketType::Pull, Options::default());
            pull.connect(ep).await?;
            count_window(&pull, duration).await;
        }
    }
    Ok(())
}

// ── fan-in (N PUSH -> 1 PULL) ────────────────────────────────────────────────

async fn run_fanin(role: Role, ep: Endpoint, duration: Duration, payload: Bytes) -> Result<()> {
    match role {
        Role::Sub => {
            let pull = Socket::new(SocketType::Pull, Options::default());
            pull.bind(ep).await?;
            count_window(&pull, duration).await;
        }
        Role::Pub => {
            let push = Socket::new(SocketType::Push, Options::default());
            push.connect(ep).await?;
            loop {
                if push.send(Message::single(payload.clone())).await.is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

// ── shared helper ───────────────────────────────────────────────────────────

/// Count received messages over `duration`, starting the clock on the first
/// message, draining bursts with try_recv, then print `THROUGHPUT count secs`.
async fn count_window(sock: &Socket, duration: Duration) {
    if sock.recv().await.is_err() {
        println!("THROUGHPUT 0 0.000001");
        return;
    }
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        match sock.recv().await {
            Ok(_) => {
                count += 1;
                while sock.try_recv().is_ok() {
                    count += 1;
                }
            }
            Err(_) => break,
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    println!("THROUGHPUT {count} {elapsed:.6}");
}
