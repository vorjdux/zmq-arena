//! zmq-arena target wrapper: omq-tokio (paddor/omq.rs, ZMTP on tokio + mio).
//!
//! All five kinds over the omq `Socket` actor API, which is wire-compatible with
//! libzmq. The runtime is built here, not via `#[tokio::main]`, so the variant
//! selects the flavor: `default` is the current-thread runtime (single), and
//! `multi_thread` is the multi-thread runtime. That mirrors omq's own bench peer,
//! which switches on OMQ_BENCH_RUNTIME.
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
use omq_tokio::{Endpoint, Message, OnMute, Options, Socket, SocketType};

/// PUB options: block when a subscriber is full rather than spinning, matching
/// omq's own bench peer.
fn sender_opts() -> Options {
    Options::default().on_mute(OnMute::Block)
}

// Note on fan-out: omq's PUSH does strict round-robin with HWM backpressure
// (block on a full peer), not libzmq-style "send to any ready peer". So on a
// shared core, a consumer that lags gates the whole rotation and the measured
// consumer's rate collapses. This is an omq backpressure characteristic, not a
// wrapper bug (a single-consumer fan-out reaches millions of msgs/s here);
// neither Block nor DropNewest helps, since dropping discards the measured peer's
// own messages. On bare metal, where each consumer has its own core and keeps up,
// the rotation never stalls. Fan-out therefore uses the engine default.

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "omq-tokio-target", version, about = "zmq-arena omq-tokio wrapper")]
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
    /// "default" (current-thread) or "multi_thread".
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

/// One-line JSON classification the orchestrator captures into each record. It is
/// variant-aware: the multi_thread variant reports `threading: multi`. The engine
/// version comes from Cargo.lock at build time (see build.rs).
fn describe(variant: &str) -> String {
    let threading = if variant == "multi_thread" { "multi" } else { "single" };
    format!(
        concat!(
            "{{\"engine\":\"omq\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"{}\",\"io\":\"epoll\"}}"
        ),
        env!("ENGINE_VERSION"),
        threading
    )
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("describe") {
        println!("{}", describe(arg_value(&args, "--variant").unwrap_or("default")));
        return Ok(());
    }

    let cli = Cli::parse();
    eprintln!(
        "omq-tokio-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role, cli.kind, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, cli.variant
    );

    let rt = if cli.variant == "multi_thread" {
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get());
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()?
    } else {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?
    };
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
        other => bail!("omq-tokio: kind '{other}' not implemented"),
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
            // this producer. omq's send queues into the socket actor, so exiting
            // right after a fixed count can drop messages still in flight; sending
            // until killed makes the consumer's count the sole stop condition.
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
            // Block (not drop) on a full subscriber so the window reflects the
            // delivered rate, matching omq's own bench peer.
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
/// message to skip the connect ramp, draining bursts with try_recv, then print
/// `THROUGHPUT count secs`.
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
