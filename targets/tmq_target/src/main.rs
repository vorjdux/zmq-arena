//! zmq-arena target wrapper: tmq (Tokio bindings over libzmq).
//!
//! tmq is not an engine. It is an async facade over the `zmq` crate (rust-zmq),
//! which binds the system libzmq, so the ZMTP implementation being measured here
//! is libzmq, same as the `libzmq` and `rust_zmq` targets. That is what makes
//! the series worth having: three points on the same engine, differing only in
//! how the caller reaches it. `libzmq` is the C++ peer, `rust_zmq` is the
//! synchronous binding, and this is that binding wrapped in futures and driven
//! by Tokio. The gaps between them are binding overhead and async wrapper
//! overhead, isolated from any protocol difference.
//!
//! Because libzmq does the multiplexing underneath, all five kinds work: a bound
//! PUSH round-robins to connecting PULL peers and a bound PULL fair-queues across
//! connecting PUSH peers, neither of which a pure-Rust engine necessarily does.
//!
//! Role and bind contract, set by the orchestrator (see targets/README.md):
//!   throughput  PULL(sub) binds,  PUSH(pub) connects
//!   latency     REP(sub) binds,   REQ(pub) connects
//!   pubsub      PUB(pub) binds,   SUB(sub) connects   (--bind on pub)
//!   fanout      PUSH(pub) binds,  PULL(sub) connects  (--bind on pub)
//!   fanin       PULL(sub) binds,  PUSH(pub) connects  (--bind on sub)
//!
//! Socket construction follows the tmq peer in the omq.rs comparison harness
//! (`scripts/tmq_bench_peer`), which builds every socket from a bare
//! `tmq::Context` with no socket options set, so this wrapper does the same
//! rather than inventing a tuning the engine's own benchmark does not use.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use futures::{SinkExt, StreamExt};
use tmq::{Context, Message, Multipart};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "tmq-target", version, about = "zmq-arena tmq wrapper")]
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
    /// Accepted and ignored: the reference tmq peer sets no socket options, so
    /// there is nothing here the arena would be justified in tuning.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record.
///
/// The engine is libzmq, reported from `zmq::version()` (the real linked
/// library, not a guess), with tmq's own version in `binding_version`. Unlike
/// the synchronous rust-zmq target this one is `concurrency: async`, which is
/// precisely the difference the pair is here to measure.
fn describe() -> String {
    let (maj, min, pat) = zmq::version();
    format!(
        concat!(
            "{{\"engine\":\"libzmq\",\"lib_version\":\"{}.{}.{}\",\"binding_version\":\"{}\",",
            "\"lib_language\":\"C++\",\"impl\":\"ffi\",\"ffi_to\":\"C\",",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"native\",\"io\":\"epoll\"}}"
        ),
        maj,
        min,
        pat,
        env!("BINDING_VERSION")
    )
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("describe") {
        println!("{}", describe());
        return Ok(());
    }
    let cli = Cli::parse();
    eprintln!(
        "tmq-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role,
        cli.kind,
        cli.transport,
        cli.endpoint,
        cli.payload_bytes,
        cli.messages,
        cli.warmup,
        cli.variant
    );

    // libzmq owns its own IO threads underneath, so the Tokio runtime here only
    // drives the futures wrapper. Multi-thread matches the reference peer, which
    // runs under #[tokio::main].
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let payload = vec![b'x'; cli.payload_bytes as usize];
    let ep = cli.endpoint.clone();
    let duration = Duration::from_secs_f64(cli.duration_secs);
    match cli.kind.as_str() {
        "throughput" => run_throughput(cli.role, &ep, cli.messages, cli.warmup, &payload).await,
        "latency" => run_latency(cli.role, &ep, cli.messages, cli.warmup, &payload).await,
        "pubsub" => run_pubsub(cli.role, &ep, duration, &payload).await,
        "fanout" => run_fanout(cli.role, &ep, duration, &payload).await,
        "fanin" => run_fanin(cli.role, &ep, duration, &payload).await,
        other => bail!("tmq: kind '{other}' not implemented"),
    }
}

fn message(payload: &[u8]) -> Message {
    Message::from(payload)
}

fn multipart(payload: &[u8]) -> Multipart {
    Multipart::from(vec![Message::from(payload)])
}

/// Duration-based kinds are TCP only, matching the orchestrator, which rejects
/// multi-peer cells on non-TCP transports.
fn require_tcp(kind: &str, endpoint: &str) -> Result<()> {
    if endpoint.starts_with("tcp://") {
        Ok(())
    } else {
        bail!("tmq {kind}: tcp only, got {endpoint}")
    }
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

/// PULL binds and is measured; PUSH connects and sends until killed. The
/// consumer drains `warmup` untimed, then times exactly `messages` and prints
/// `THROUGHPUT <count> <elapsed>`, so process spawn and the connect ramp stay
/// out of the rate.
async fn run_throughput(
    role: Role,
    ep: &str,
    messages: u64,
    warmup: u64,
    payload: &[u8],
) -> Result<()> {
    match role {
        Role::Sub => {
            let ctx = Context::new();
            let mut pull = tmq::pull(&ctx).bind(ep)?;
            recv_measured(&mut pull, warmup, messages).await;
        }
        Role::Pub => {
            let ctx = Context::new();
            let mut push = tmq::push(&ctx).connect(ep)?;
            // Send until the orchestrator kills this side: the consumer's count
            // is the sole stop condition, so a producer that exits on a fixed
            // count cannot strand the measurement.
            while push.send(message(payload)).await.is_ok() {}
        }
    }
    Ok(())
}

/// Receive `warmup` untimed, then time `messages` and print the rate line.
async fn recv_measured<S>(stream: &mut S, warmup: u64, messages: u64)
where
    S: futures::Stream<Item = tmq::Result<Multipart>> + Unpin,
{
    for _ in 0..warmup {
        if stream.next().await.is_none() {
            println!("THROUGHPUT 0 0.000001");
            return;
        }
    }
    let t0 = Instant::now();
    let mut measured = 0u64;
    while measured < messages {
        match stream.next().await {
            Some(Ok(_)) => measured += 1,
            _ => break,
        }
    }
    let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
    println!("THROUGHPUT {measured} {elapsed:.6}");
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

/// tmq models REQ/REP as a type-state machine: sending yields the receiver and
/// receiving yields the next sender, which is how it enforces strict
/// alternation. The loops below thread that state rather than reusing a socket.
async fn run_latency(
    role: Role,
    ep: &str,
    messages: u64,
    warmup: u64,
    payload: &[u8],
) -> Result<()> {
    match role {
        Role::Sub => {
            let ctx = Context::new();
            let mut receiver = tmq::reply(&ctx).bind(ep)?;
            // Echo until the REQ side goes away, which is the normal end of the
            // cell.
            while let Ok((msg, sender)) = receiver.recv().await {
                match sender.send(msg).await {
                    Ok(next) => receiver = next,
                    Err(_) => break,
                }
            }
        }
        Role::Pub => {
            let ctx = Context::new();
            let mut sender = tmq::request(&ctx).connect(ep)?;
            for _ in 0..warmup {
                let receiver = sender.send(multipart(payload)).await?;
                let (_, next) = receiver.recv().await?;
                sender = next;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                let receiver = sender.send(multipart(payload)).await?;
                let (_, next) = receiver.recv().await?;
                sender = next;
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

// ── pub/sub, fan-out, fan-in ────────────────────────────────────────────────

async fn run_pubsub(role: Role, ep: &str, duration: Duration, payload: &[u8]) -> Result<()> {
    require_tcp("pubsub", ep)?;
    let ctx = Context::new();
    match role {
        Role::Pub => {
            let mut publisher = tmq::publish(&ctx).bind(ep)?;
            while publisher.send(multipart(payload)).await.is_ok() {}
        }
        Role::Sub => {
            let mut sub = tmq::subscribe(&ctx).connect(ep)?.subscribe(b"")?;
            count_window(&mut sub, duration).await;
        }
    }
    Ok(())
}

async fn run_fanout(role: Role, ep: &str, duration: Duration, payload: &[u8]) -> Result<()> {
    require_tcp("fanout", ep)?;
    let ctx = Context::new();
    match role {
        Role::Pub => {
            let mut push = tmq::push(&ctx).bind(ep)?;
            while push.send(message(payload)).await.is_ok() {}
        }
        Role::Sub => {
            let mut pull = tmq::pull(&ctx).connect(ep)?;
            count_window(&mut pull, duration).await;
        }
    }
    Ok(())
}

async fn run_fanin(role: Role, ep: &str, duration: Duration, payload: &[u8]) -> Result<()> {
    require_tcp("fanin", ep)?;
    let ctx = Context::new();
    match role {
        Role::Sub => {
            let mut pull = tmq::pull(&ctx).bind(ep)?;
            count_window(&mut pull, duration).await;
        }
        Role::Pub => {
            let mut push = tmq::push(&ctx).connect(ep)?;
            while push.send(message(payload)).await.is_ok() {}
        }
    }
    Ok(())
}

/// Count received messages over `duration`, starting the clock on the first
/// message so the connect ramp is excluded, then print `THROUGHPUT count secs`.
async fn count_window<S>(stream: &mut S, duration: Duration)
where
    S: futures::Stream<Item = tmq::Result<Multipart>> + Unpin,
{
    if !matches!(stream.next().await, Some(Ok(_))) {
        println!("THROUGHPUT 0 0.000001");
        return;
    }
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        match stream.next().await {
            Some(Ok(_)) => count += 1,
            _ => break,
        }
    }
    let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
    println!("THROUGHPUT {count} {elapsed:.6}");
}
