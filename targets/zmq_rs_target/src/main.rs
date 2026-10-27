//! zmq-arena target wrapper: zmq-rs (trevorbernard/zmq-rs, a pure-Rust port of
//! libzmq).
//!
//! The blocking API over the engine's own IO threads, which is the only
//! execution model it ships: there is no runtime to pick, and the `_async`
//! methods poll the same functions the blocking ones block on, under whatever
//! executor the caller brings. Measuring them would measure that executor.
//!
//! Role and bind contract, set by the orchestrator (see targets/README.md):
//!   throughput  PULL(sub) binds,  PUSH(pub) connects
//!   latency     REP(sub) binds,   REQ(pub) connects
//!   pubsub      PUB(pub) binds,   SUB(sub) connect    (--bind on pub)
//!   fanout      PUSH(pub) binds,  PULL(sub) connect   (--bind on pub)
//!   fanin       PULL(sub) binds,  PUSH(pub) connect   (--bind on sub)
//!
//! Knobs: `io_threads` (on the context), `sndhwm` and `rcvhwm` (on the socket),
//! matching the libzmq targets. 0 means unlimited, as in libzmq.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use zmq::{Context, Frame, Hwm, Message, Pub, Pull, Push, Rep, Req, SocketOptions, Sub};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "zmq-rs-target", version, about = "zmq-arena zmq-rs wrapper")]
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
/// engine version and locked commit come from Cargo.lock at build time (see
/// build.rs). `threading` is `native` as for libzmq: OS threads the engine owns.
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"zmq-rs\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"sync\",\"threading\":\"native\",\"io\":\"epoll\"}}"
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
        "zmq-rs-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role,
        cli.kind,
        cli.transport,
        cli.endpoint,
        cli.payload_bytes,
        cli.messages,
        cli.warmup,
        cli.variant
    );

    let ctx = make_context(&knobs)?;
    let opts = socket_options(&knobs)?;
    let payload = vec![b'x'; cli.payload_bytes as usize];
    let duration = Duration::from_secs_f64(cli.duration_secs);
    match cli.kind.as_str() {
        "throughput" => run_throughput(&cli, &ctx, opts, &payload),
        "latency" => run_latency(&cli, &ctx, opts, &payload),
        "pubsub" => run_pubsub(&cli, &ctx, opts, duration, &payload),
        "fanout" => run_fanout(&cli, &ctx, opts, duration, &payload),
        "fanin" => run_fanin(&cli, &ctx, opts, duration, &payload),
        other => bail!("zmq-rs: kind '{other}' not implemented"),
    }
}

// ── context and socket option knobs ─────────────────────────────────────────

fn make_context(knobs: &BTreeMap<String, String>) -> Result<Context> {
    match knobs.get("io_threads") {
        Some(v) => Ok(Context::with_io_threads(v.parse().context("io_threads")?)?),
        None => Ok(Context::new()?),
    }
}

fn hwm(knobs: &BTreeMap<String, String>, key: &str, default: Hwm) -> Result<Hwm> {
    let Some(v) = knobs.get(key) else {
        return Ok(default);
    };
    let n: usize = v.parse().with_context(|| format!("{key}={v}"))?;
    Ok(NonZeroUsize::new(n).map_or(Hwm::Unlimited, Hwm::Messages))
}

fn socket_options(knobs: &BTreeMap<String, String>) -> Result<SocketOptions> {
    let defaults = SocketOptions::default();
    Ok(SocketOptions {
        send_hwm: hwm(knobs, "sndhwm", defaults.send_hwm)?,
        recv_hwm: hwm(knobs, "rcvhwm", defaults.recv_hwm)?,
        ..defaults
    })
}

/// A fresh copy of the payload per send, as `zmq_send` copies its buffer for the
/// libzmq targets and as zmq-rs's own perf bench builds each message. Cloning one
/// prebuilt `Message` would share a refcounted buffer instead, which no other
/// wrapper over a copying API gets to do.
fn message(payload: &[u8]) -> Message {
    Message::from(Frame::copy_from_slice(payload))
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

/// PUSH sends `messages + warmup` and returns; dropping the context lingers
/// until the queue is delivered (`ZMQ_LINGER` defaults to forever). PULL drains
/// the warmup prefix untimed, then times the measured block only.
fn run_throughput(cli: &Cli, ctx: &Context, opts: SocketOptions, payload: &[u8]) -> Result<()> {
    match cli.role {
        Role::Sub => {
            let mut pull = ctx.socket_with::<Pull>(opts);
            pull.bind(&cli.endpoint)
                .with_context(|| format!("bind {}", cli.endpoint))?;
            for _ in 0..cli.warmup {
                pull.recv();
            }
            let t0 = Instant::now();
            for _ in 0..cli.messages {
                pull.recv();
            }
            let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
            println!("THROUGHPUT {} {elapsed:.6}", cli.messages);
        }
        Role::Pub => {
            let mut push = ctx.socket_with::<Push>(opts);
            push.connect(&cli.endpoint)
                .with_context(|| format!("connect {}", cli.endpoint))?;
            for _ in 0..cli.messages + cli.warmup {
                push.send(message(payload));
            }
        }
    }
    Ok(())
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

/// REP binds and echoes the request body until killed. REQ connects, runs
/// `warmup` untimed round-trips, then times `messages` and prints the quantiles.
fn run_latency(cli: &Cli, ctx: &Context, opts: SocketOptions, payload: &[u8]) -> Result<()> {
    match cli.role {
        Role::Sub => {
            let mut rep = ctx.socket_with::<Rep>(opts);
            rep.bind(&cli.endpoint)
                .with_context(|| format!("bind {}", cli.endpoint))?;
            loop {
                let request = rep.recv();
                let body = request.body().clone();
                request.reply(body);
            }
        }
        Role::Pub => {
            let mut req = ctx.socket_with::<Req>(opts);
            req.connect(&cli.endpoint)
                .with_context(|| format!("connect {}", cli.endpoint))?;
            let mut round_trip = || {
                req.request(message(payload))
                    .map_err(|e| anyhow!("request: {e:?}"))
            };
            for _ in 0..cli.warmup {
                round_trip()?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(cli.messages as usize);
            for _ in 0..cli.messages {
                let t = Instant::now();
                round_trip()?;
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

/// PUB binds and broadcasts until killed. `send_all` blocks until every
/// subscriber has room (libzmq's `ZMQ_XPUB_NODROP`) rather than `send`, which
/// drops for a full subscriber: the protocol rules count HWM drops on pub/sub as
/// a failed cell, and a lossy publisher lets the fast subscribers report a rate
/// the slow ones never saw. omq's wrapper sets `xpub_nodrop` for the same reason.
fn run_pubsub(
    cli: &Cli,
    ctx: &Context,
    opts: SocketOptions,
    duration: Duration,
    payload: &[u8],
) -> Result<()> {
    match cli.role {
        Role::Pub => {
            let mut publisher = ctx.socket_with::<Pub>(opts);
            publisher
                .bind(&cli.endpoint)
                .with_context(|| format!("bind {}", cli.endpoint))?;
            loop {
                publisher.send_all(message(payload));
            }
        }
        Role::Sub => {
            let mut sub = ctx.socket_with::<Sub>(opts);
            sub.subscribe(b"");
            sub.connect(&cli.endpoint)
                .with_context(|| format!("connect {}", cli.endpoint))?;
            count_window(duration, |t| sub.recv_timeout(t).is_some());
        }
    }
    Ok(())
}

// ── fan-out (1 PUSH -> N PULL) ───────────────────────────────────────────────

fn run_fanout(
    cli: &Cli,
    ctx: &Context,
    opts: SocketOptions,
    duration: Duration,
    payload: &[u8],
) -> Result<()> {
    match cli.role {
        Role::Pub => {
            let mut push = ctx.socket_with::<Push>(opts);
            push.bind(&cli.endpoint)
                .with_context(|| format!("bind {}", cli.endpoint))?;
            loop {
                push.send(message(payload));
            }
        }
        Role::Sub => {
            let mut pull = ctx.socket_with::<Pull>(opts);
            pull.connect(&cli.endpoint)
                .with_context(|| format!("connect {}", cli.endpoint))?;
            count_window(duration, |t| pull.recv_timeout(t).is_some());
        }
    }
    Ok(())
}

// ── fan-in (N PUSH -> 1 PULL) ────────────────────────────────────────────────

fn run_fanin(
    cli: &Cli,
    ctx: &Context,
    opts: SocketOptions,
    duration: Duration,
    payload: &[u8],
) -> Result<()> {
    match cli.role {
        Role::Sub => {
            let mut pull = ctx.socket_with::<Pull>(opts);
            pull.bind(&cli.endpoint)
                .with_context(|| format!("bind {}", cli.endpoint))?;
            count_window(duration, |t| pull.recv_timeout(t).is_some());
        }
        Role::Pub => {
            let mut push = ctx.socket_with::<Push>(opts);
            push.connect(&cli.endpoint)
                .with_context(|| format!("connect {}", cli.endpoint))?;
            loop {
                push.send(message(payload));
            }
        }
    }
    Ok(())
}

// ── shared helper ───────────────────────────────────────────────────────────

/// Count received messages over `duration`, starting the clock on the first
/// message to skip the connect ramp, then print `THROUGHPUT count secs`. The
/// receive is bounded, as libzmq's `ZMQ_RCVTIMEO` bounds it in the libzmq
/// targets, so a stalled stream ends at the deadline instead of running past it
/// and understating its own rate.
fn count_window(duration: Duration, mut recv: impl FnMut(Duration) -> bool) {
    const TICK: Duration = Duration::from_millis(200);
    while !recv(TICK) {}
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        if recv(TICK) {
            count += 1;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    println!("THROUGHPUT {count} {elapsed:.6}");
}
