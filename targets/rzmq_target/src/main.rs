//! zmq-arena target wrapper: rzmq (pure-Rust ZMTP, epoll or `io_uring`).
//!
//! All five kinds over rzmq's `Context`/`Socket` API, which is shaped like
//! libzmq's: create a socket from a context, bind or connect an endpoint
//! string, then `send`/`recv` whole messages.
//!
//! rzmq ships two IO backends and the arena measures both, the same way it
//! measures every runtime an engine offers:
//!
//!   `--variant default`   epoll, the stock configuration
//!   `--variant io_uring`  the `io_uring` session, with zero-copy send and
//!                         multishot receive enabled
//!
//! The `io_uring` options are the three the rzmq peer in the omq.rs comparison
//! harness sets (`scripts/rzmq_bench_peer`), so the two projects configure the
//! backend the same way and their numbers can be read against each other.
//!
//! IPC note: rzmq uses pathname IPC (`ipc:///tmp/...`), not abstract-namespace
//! sockets, so the matrix endpoint must be a real path.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use rzmq::socket::options::{
    IO_URING_RCVMULTISHOT, IO_URING_SESSION_ENABLED, IO_URING_SNDZEROCOPY, SUBSCRIBE,
};
use rzmq::{Context, Msg, Socket, SocketType};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "rzmq-target", version, about = "zmq-arena rzmq wrapper")]
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
    /// Accepted and ignored: the backend is chosen by `--variant`, and the
    /// reference peer sets no other socket options.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record.
/// The IO model is the variant's, not the build's: one binary drives both.
fn describe(variant: &str) -> String {
    let io = if variant == "io_uring" {
        "io_uring"
    } else {
        "epoll"
    };
    format!(
        concat!(
            "{{\"engine\":\"rzmq\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"multi\",\"io\":\"{}\"}}"
        ),
        env!("ENGINE_VERSION"),
        io
    )
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("describe") {
        println!(
            "{}",
            describe(arg_value(&args, "--variant").unwrap_or("default"))
        );
        return Ok(());
    }
    let cli = Cli::parse();
    eprintln!(
        "rzmq-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role,
        cli.kind,
        cli.transport,
        cli.endpoint,
        cli.payload_bytes,
        cli.messages,
        cli.warmup,
        cli.variant
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cli))
}

/// Apply the `io_uring` session options for that variant, and nothing
/// for the default one. Set before bind/connect so the session is in place when
/// the transport is created.
async fn configure(sock: &Socket, variant: &str) -> Result<()> {
    if variant != "io_uring" {
        return Ok(());
    }
    for opt in [
        IO_URING_SESSION_ENABLED,
        IO_URING_SNDZEROCOPY,
        IO_URING_RCVMULTISHOT,
    ] {
        sock.set_option_raw(opt, &1i32.to_ne_bytes()).await?;
    }
    Ok(())
}

async fn socket(ctx: &Context, kind: SocketType, variant: &str) -> Result<Socket> {
    let s = ctx.socket(kind)?;
    configure(&s, variant).await?;
    Ok(s)
}

async fn run(cli: Cli) -> Result<()> {
    let payload = vec![b'x'; cli.payload_bytes as usize];
    let ep = cli.endpoint.clone();
    let v = cli.variant.clone();
    let duration = Duration::from_secs_f64(cli.duration_secs);
    let ctx = Context::new()?;
    match cli.kind.as_str() {
        "throughput" => {
            throughput(&ctx, cli.role, &ep, &v, cli.messages, cli.warmup, &payload).await
        }
        "latency" => latency(&ctx, cli.role, &ep, &v, cli.messages, cli.warmup, &payload).await,
        "pubsub" => pubsub(&ctx, cli.role, &ep, &v, duration, &payload).await,
        "fanout" => fanout(&ctx, cli.role, &ep, &v, duration, &payload).await,
        "fanin" => fanin(&ctx, cli.role, &ep, &v, duration, &payload).await,
        other => bail!("rzmq: kind '{other}' not implemented"),
    }
}

fn msg(payload: &[u8]) -> Msg {
    Msg::from_vec(payload.to_vec())
}

/// Duration-based kinds are TCP only, matching the orchestrator.
fn require_tcp(kind: &str, ep: &str) -> Result<()> {
    if ep.starts_with("tcp://") {
        Ok(())
    } else {
        bail!("rzmq {kind}: tcp only, got {ep}")
    }
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

/// PULL binds and is measured; PUSH connects and sends until killed. The
/// consumer drains `warmup` untimed, then times exactly `messages`, so process
/// spawn and the connect ramp stay out of the rate.
async fn throughput(
    ctx: &Context,
    role: Role,
    ep: &str,
    variant: &str,
    messages: u64,
    warmup: u64,
    payload: &[u8],
) -> Result<()> {
    match role {
        Role::Sub => {
            let pull = socket(ctx, SocketType::Pull, variant).await?;
            pull.bind(ep).await?;
            for _ in 0..warmup {
                pull.recv().await?;
            }
            let t0 = Instant::now();
            let mut n = 0u64;
            while n < messages {
                pull.recv().await?;
                n += 1;
            }
            report(n, t0.elapsed());
        }
        Role::Pub => {
            let push = socket(ctx, SocketType::Push, variant).await?;
            push.connect(ep).await?;
            // Send until killed: exiting on a fixed count can strand messages
            // still queued, so the consumer's count is the only stop condition.
            while push.send(msg(payload)).await.is_ok() {}
        }
    }
    Ok(())
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

async fn latency(
    ctx: &Context,
    role: Role,
    ep: &str,
    variant: &str,
    messages: u64,
    warmup: u64,
    payload: &[u8],
) -> Result<()> {
    match role {
        Role::Sub => {
            let rep = socket(ctx, SocketType::Rep, variant).await?;
            rep.bind(ep).await?;
            // Echo until the REQ side goes away, the normal end of the cell.
            while let Ok(m) = rep.recv().await {
                if rep.send(m).await.is_err() {
                    break;
                }
            }
        }
        Role::Pub => {
            let req = socket(ctx, SocketType::Req, variant).await?;
            req.connect(ep).await?;
            for _ in 0..warmup {
                req.send(msg(payload)).await?;
                req.recv().await?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                req.send(msg(payload)).await?;
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

// ── pub/sub, fan-out, fan-in ────────────────────────────────────────────────

async fn pubsub(
    ctx: &Context,
    role: Role,
    ep: &str,
    variant: &str,
    duration: Duration,
    payload: &[u8],
) -> Result<()> {
    require_tcp("pubsub", ep)?;
    match role {
        Role::Pub => {
            let publisher = socket(ctx, SocketType::Pub, variant).await?;
            publisher.bind(ep).await?;
            while publisher.send(msg(payload)).await.is_ok() {}
        }
        Role::Sub => {
            let sub = socket(ctx, SocketType::Sub, variant).await?;
            sub.connect(ep).await?;
            // Option 6 is SUBSCRIBE; an empty prefix takes every topic.
            sub.set_option(SUBSCRIBE, b"").await?;
            count_window(&sub, duration).await;
        }
    }
    Ok(())
}

async fn fanout(
    ctx: &Context,
    role: Role,
    ep: &str,
    variant: &str,
    duration: Duration,
    payload: &[u8],
) -> Result<()> {
    require_tcp("fanout", ep)?;
    match role {
        Role::Pub => {
            let push = socket(ctx, SocketType::Push, variant).await?;
            push.bind(ep).await?;
            while push.send(msg(payload)).await.is_ok() {}
        }
        Role::Sub => {
            let pull = socket(ctx, SocketType::Pull, variant).await?;
            pull.connect(ep).await?;
            count_window(&pull, duration).await;
        }
    }
    Ok(())
}

async fn fanin(
    ctx: &Context,
    role: Role,
    ep: &str,
    variant: &str,
    duration: Duration,
    payload: &[u8],
) -> Result<()> {
    require_tcp("fanin", ep)?;
    match role {
        Role::Sub => {
            let pull = socket(ctx, SocketType::Pull, variant).await?;
            pull.bind(ep).await?;
            count_window(&pull, duration).await;
        }
        Role::Pub => {
            let push = socket(ctx, SocketType::Push, variant).await?;
            push.connect(ep).await?;
            while push.send(msg(payload)).await.is_ok() {}
        }
    }
    Ok(())
}

/// Count received messages over `duration`, starting the clock on the first
/// message so the connect ramp is excluded.
async fn count_window(sock: &Socket, duration: Duration) {
    if sock.recv().await.is_err() {
        println!("THROUGHPUT 0 0.000001");
        return;
    }
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        if sock.recv().await.is_err() {
            break;
        }
        count += 1;
    }
    report(count, t0.elapsed());
}

fn report(count: u64, elapsed: Duration) {
    println!("THROUGHPUT {count} {:.6}", elapsed.as_secs_f64().max(1e-9));
}
