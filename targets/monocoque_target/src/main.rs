//! zmq-arena target wrapper: monocoque (monocoque-rs 0.4.0, ZMTP 3.1).
//!
//! All five kinds over TCP and IPC, tuned against monocoque's own bench peer in
//! the omq.rs comparison harness (`scripts/monocoque_bench_peer`) so the numbers
//! reflect a competently configured engine rather than the untuned defaults:
//!   - bulk streams (throughput, fan-out, fan-in) read a full 64 KiB slab carve
//!     and write coalesced, which is monocoque's main throughput lever;
//!   - REQ/REP disables coalescing so a request leaves eagerly instead of
//!     waiting to batch, and leaves the read carve at the default;
//!   - every receive path uses the allocation-free `recv_into` family into one
//!     reused buffer; PUSH sends via `send_one` and PUB via `send_frames`, both
//!     of which avoid the per-message `Vec` that plain `send` allocates. REQ is
//!     the exception: its `send` takes the frames by value with no single-frame
//!     form, so the latency loop still clones one Vec per round trip;
//!   - REQ times each round-trip and prints the quantiles the orchestrator
//!     parses; REP echoes.
//!
//! The orchestrator spawns the consumer (binds) first, then the producer
//! (connects). monocoque's runtime is a compile-time choice, so this wrapper
//! builds once per runtime the engine ships and reports each as its own variant:
//! `compio` (`io_uring`, the default), `tokio` (epoll) and `smol` (epoll). The
//! socket loops below are identical across all three because they go through
//! monocoque's `rt` facade, so the only thing that differs between the series is
//! the IO model, which is the comparison worth having.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use bytes::Bytes;
use clap::{Parser, ValueEnum};
use compio_io::{AsyncRead, AsyncWrite};
use monocoque::SocketOptions;
use monocoque::rt::{LocalRuntime, TcpListener, TcpStream, UnixListener, UnixStream};
use monocoque::zmq::{
    PubSocket, PullFanIn, PullSocket, PushFanOut, PushSocket, RepSocket, ReqSocket, SubSocket,
};

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
#[command(
    name = "monocoque-target",
    version,
    about = "zmq-arena monocoque wrapper"
)]
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
    /// `key=value` tuning knobs, recorded in the matrix so a configuration is
    /// reproducible from the run file. Recognised: `pub_workers=<n>`.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record. The
/// runtime is a compile-time choice (`runtime-compio` / `runtime-tokio` /
/// `runtime-smol`), so `describe` reports whichever backend this binary was
/// built with. All three are single-threaded: monocoque's sockets are !Send and
/// its runtime is thread-per-core, so the tokio backend runs current-thread in a
/// `LocalSet` and smol drives a thread-local executor. The engine version is
/// read from Cargo.lock at build time (build.rs).
fn describe() -> String {
    // One backend is selected at compile time. compio drives io_uring; tokio and
    // smol both drive epoll on Linux, through mio and polling respectively.
    #[cfg(feature = "compio")]
    let io = "io_uring";
    #[cfg(any(feature = "tokio", feature = "smol"))]
    let io = "epoll";
    format!(
        concat!(
            "{{\"engine\":\"monocoque\",\"lib_version\":\"{}\",\"binding_version\":null,",
            "\"lib_language\":\"Rust\",\"impl\":\"native\",\"ffi_to\":null,",
            "\"language\":\"Rust\",\"concurrency\":\"async\",\"threading\":\"single\",\"io\":\"{}\"}}"
        ),
        env!("ENGINE_VERSION"),
        io
    )
}

/// Look up a `key=value` knob. Unknown keys are ignored rather than rejected, so
/// a matrix can carry a knob for one engine without breaking the others.
fn knob<'a>(knobs: &'a [String], key: &str) -> Option<&'a str> {
    knobs
        .iter()
        .filter_map(|k| k.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("describe") {
        println!("{}", describe());
        return Ok(());
    }
    let cli = Cli::parse();
    eprintln!(
        "monocoque-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role,
        cli.kind,
        cli.transport,
        cli.endpoint,
        cli.payload_bytes,
        cli.messages,
        cli.warmup,
        cli.variant
    );

    let ep = parse_endpoint(&cli.endpoint)?;
    let payload = Bytes::from(vec![b'x'; cli.payload_bytes as usize]);
    let role = cli.role;
    let kind = cli.kind.clone();
    let (messages, warmup) = (cli.messages, cli.warmup);
    let peers = cli.peers.unwrap_or(1).max(1);
    let duration = Duration::from_secs_f64(cli.duration_secs);
    // PUB fans out from a pool of worker threads. The library default is the CPU
    // count clamped to [2, 16], which on a pinned cpuset oversubscribes the cell
    // and makes the number depend on the host rather than the engine, so the
    // matrix pins it. `pub_workers=0` asks for the library default instead.
    let pub_workers = knob(&cli.knobs, "pub_workers")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1);

    // monocoque::rt::LocalRuntime is the runtime-agnostic driver: compio's
    // io_uring runtime or tokio's current-thread runtime, per the compiled
    // feature. The socket loops below are identical across backends because they
    // use monocoque's rt net types.
    LocalRuntime::new()?.block_on(async move {
        match kind.as_str() {
            "throughput" => run_throughput(role, ep, messages, warmup, &payload).await,
            "latency" => run_latency(role, ep, messages, warmup, &payload).await,
            "pubsub" => run_pubsub(role, ep, peers, duration, &payload, pub_workers).await,
            "fanout" => run_fanout(role, ep, peers, duration, &payload).await,
            "fanin" => run_fanin(role, ep, peers, duration, &payload).await,
            other => bail!("monocoque: kind '{other}' not implemented"),
        }
    })?;
    Ok(())
}

// ── per-workload socket setup ───────────────────────────────────────────────
//
// monocoque does not infer a buffer/coalescing profile from the workload; it
// exposes the knobs and expects the caller to set them for the traffic. These
// profiles match the tuned monocoque peer in the omq.rs comparison harness, so
// zmq-arena and OMQ configure the engine the same way and the two sets of
// numbers can be read against each other.
//
// A note on read buffers, because the obvious tuning is wrong here. In 0.4.0 the
// read buffer is not an allocation: it is the size of the carve taken from a
// shared 64 KiB read slab, values are clamped into `64..=65536`, and a no-timeout
// read trims the carve to the bytes actually read and hands the tail back. The
// default is already 32 KiB. So asking for a 1 MiB "payload-sized" buffer, as
// this target did, silently clamps to 64 KiB, and asking for 16 KiB is *below*
// the default and halves the read batch. There is exactly one useful bulk value,
// the full slab, and no reason to scale it with the payload.

/// monocoque's shared read slab, and therefore the ceiling on `read_buffer_size`.
/// A bulk receiver wants the whole thing: it is the largest batch a single read
/// can carve, and the trim-and-return behaviour means an idle receiver does not
/// pay for asking.
const READ_SLAB: usize = 64 * 1024;

/// Write buffer for the bulk streams. Unlike the read side this is a real
/// `BytesMut` capacity, and it doubles as the coalesce threshold, so it sets how
/// many frames pack into one write submission.
const BULK_WRITE_BUF: usize = 64 * 1024;

/// Sender profile for the bulk streams (throughput, fan-out, fan-in producers):
/// full-slab reads, a 64 KiB coalescing write buffer, and write coalescing on.
/// Coalescing is the single biggest throughput lever monocoque exposes; 0.4.0
/// extended it from PUSH to every connected socket type.
fn bulk_send_opts() -> SocketOptions {
    SocketOptions::default()
        .with_buffer_sizes(READ_SLAB, BULK_WRITE_BUF)
        .with_write_coalescing(true)
        .with_write_coalesce_threshold(BULK_WRITE_BUF)
}

/// Receiver profile for the bulk streams: full-slab read carves, default write
/// buffer (a bulk receiver sends nothing but the handshake).
fn bulk_recv_opts() -> SocketOptions {
    SocketOptions::default().with_buffer_sizes(READ_SLAB, 8 * 1024)
}

/// Request/reply profile. Coalescing is explicitly off (it is off by default, but
/// stating it documents the intent): a REQ that waits to batch would be measuring
/// the batch timer, not the round-trip. Buffers stay at the defaults, since the
/// read carve is trimmed to what arrives and a one-message write needs no room.
fn latency_opts() -> SocketOptions {
    SocketOptions::default().with_write_coalescing(false)
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

/// PUSH/PULL throughput. PUB sends `messages + warmup` messages; SUB receives the
/// `warmup` prefix untimed, then times only the `messages` steady-state block and
/// prints `THROUGHPUT <messages> <elapsed_secs>`. Timing the measured block inside
/// the target (not the orchestrator's wall clock) keeps process spawn, the
/// connection handshake, and the warmup transfer out of the rate.
async fn run_throughput(
    role: Role,
    ep: Endpoint,
    messages: u64,
    warmup: u64,
    payload: &Bytes,
) -> Result<()> {
    let total = messages + warmup;
    match (role, ep) {
        (Role::Pub, Endpoint::Tcp(addr)) => {
            let mut push = PushSocket::connect_with_options(addr, bulk_send_opts()).await?;
            send_block(&mut push, total, payload).await?;
        }
        (Role::Pub, Endpoint::Ipc(path)) => {
            let stream = UnixStream::connect(&path).await?;
            let mut push =
                PushSocket::from_unix_stream_with_options(stream, bulk_send_opts()).await?;
            send_block(&mut push, total, payload).await?;
        }
        (Role::Sub, Endpoint::Tcp(addr)) => {
            let listener = TcpListener::bind(addr).await?;
            let (stream, _) = listener.accept().await?;
            let mut pull = PullSocket::from_tcp_with_options(stream, bulk_recv_opts()).await?;
            recv_measured(&mut pull, warmup, messages).await?;
        }
        (Role::Sub, Endpoint::Ipc(path)) => {
            let listener = UnixListener::bind(&path).await?;
            let (stream, _) = listener.accept().await?;
            let mut pull =
                PullSocket::from_unix_stream_with_options(stream, bulk_recv_opts()).await?;
            recv_measured(&mut pull, warmup, messages).await?;
        }
    }
    Ok(())
}

async fn send_block<S>(push: &mut PushSocket<S>, total: u64, payload: &Bytes) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut i = 0u64;
    while i < total {
        // send_one takes a single frame by value: no per-message Vec. `send`
        // allocates a one-element Vec on every call, which at these rates is a
        // measurable share of the loop.
        push.send_one(payload.clone()).await?;
        i += 1;
        if i.is_multiple_of(64) {
            push.flush().await?;
        }
    }
    push.flush().await?; // flush the last partial batch
    Ok(())
}

/// Receive `warmup` messages untimed, then time the receipt of `messages` and
/// print `THROUGHPUT <messages> <elapsed_secs>`. The timer starts only once the
/// warmup prefix has drained, so the reported rate is the steady-state window.
async fn recv_measured<S>(pull: &mut PullSocket<S>, warmup: u64, messages: u64) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf: Vec<Bytes> = Vec::with_capacity(4);
    let total = warmup + messages;
    let mut count = 0u64;
    let mut t0: Option<Instant> = None;
    while count < total {
        if pull.recv_into(&mut buf).await? {
            count += 1;
            if t0.is_none() && count >= warmup {
                t0 = Some(Instant::now()); // warmup drained; start the clock
            }
            while count < total {
                if pull.try_recv_into(&mut buf)? {
                    count += 1;
                    if t0.is_none() && count >= warmup {
                        t0 = Some(Instant::now());
                    }
                } else {
                    break;
                }
            }
        } else {
            break;
        }
    }
    let elapsed = t0.map_or(1e-6, |t| t.elapsed().as_secs_f64()).max(1e-9);
    let measured = count.saturating_sub(warmup);
    println!("THROUGHPUT {measured} {elapsed:.6}");
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
            let mut rep = RepSocket::from_tcp_with_options(stream, latency_opts()).await?;
            echo_loop(&mut rep).await?;
        }
        (Role::Sub, Endpoint::Ipc(path)) => {
            let listener = UnixListener::bind(&path).await?;
            let (stream, _) = listener.accept().await?;
            let mut rep = RepSocket::from_unix_stream_with_options(stream, latency_opts()).await?;
            echo_loop(&mut rep).await?;
        }
        (Role::Pub, Endpoint::Tcp(addr)) => {
            let stream = TcpStream::connect(addr).await?;
            let mut req = ReqSocket::from_tcp_with_options(stream, latency_opts()).await?;
            req_measure(&mut req, messages, warmup, payload).await?;
        }
        (Role::Pub, Endpoint::Ipc(path)) => {
            let stream = UnixStream::connect(&path).await?;
            let mut req = ReqSocket::from_unix_stream_with_options(stream, latency_opts()).await?;
            req_measure(&mut req, messages, warmup, payload).await?;
        }
    }
    Ok(())
}

async fn echo_loop<S>(rep: &mut RepSocket<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
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
    S: AsyncRead + AsyncWrite + Unpin,
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
    pub_workers: usize,
) -> Result<()> {
    let addr = match ep {
        Endpoint::Tcp(a) => a,
        Endpoint::Ipc(_) => bail!("monocoque pubsub: tcp only for now"),
    };
    match role {
        Role::Pub => {
            // 0.4.0 gives PUB no way to set write coalescing or buffer sizes
            // (the broadcast path does not read those options at all), so the
            // worker count is the only publisher-side lever there is. Say so
            // plainly rather than pretending the PUB side is tuned like PUSH.
            let mut publisher = if pub_workers == 0 {
                PubSocket::bind(&addr.to_string()).await?
            } else {
                PubSocket::bind_with_workers(&addr.to_string(), pub_workers).await?
            };
            for _ in 0..peers {
                publisher.accept_subscriber().await?;
            }
            // Let every subscription propagate before oversending, so the stream is
            // live and the PUB is not publishing into a not-yet-subscribed peer.
            // Blocking sleep, matching the library's own pub/sub bench: this is a
            // one-time settle before the send loop, and the async runtime timer is
            // not guaranteed to be driven here.
            std::thread::sleep(Duration::from_millis(200));
            loop {
                // send_frames publishes from a borrowed slice: no per-message
                // Vec allocation or Bytes clone. `send(vec![payload.clone()])`
                // allocated and cloned on every broadcast, which the library's
                // benchmark avoids.
                let _ = publisher.send_frames(std::slice::from_ref(payload)).await;
            }
        }
        Role::Sub => {
            // Build the SUB over a raw TCP stream so it gets the bulk receive
            // profile. The bare SubSocket::connect path takes the default carve,
            // which reads the broadcast stream in smaller chunks.
            let stream = TcpStream::connect(addr).await?;
            let mut sub = SubSocket::from_tcp_with_options(stream, bulk_recv_opts()).await?;
            sub.subscribe(b"").await?;
            // 0.4.0 ported the allocation-free receive path from PULL to every
            // socket, SUB included, so the counting loop reuses one buffer.
            let mut buf: Vec<Bytes> = Vec::with_capacity(4);
            if !sub.recv_into(&mut buf).await.unwrap_or(false) {
                println!("THROUGHPUT 0 0.000001");
                return Ok(());
            }
            let mut count: u64 = 1;
            let t0 = Instant::now();
            let deadline = t0 + duration;
            while Instant::now() < deadline {
                match sub.recv_into(&mut buf).await {
                    Ok(true) => count += 1,
                    _ => break,
                }
            }
            let elapsed = t0.elapsed().as_secs_f64();
            println!("THROUGHPUT {count} {elapsed:.6}");
        }
    }
    Ok(())
}

// ── fan-out (1 PUSH -> N PULL) ───────────────────────────────────────────────

/// The producer binds a `PushFanOut` ventilator that accepts `peers` PULL workers
/// and round-robins forever, flushing every 64 sends per worker. Each consumer
/// connects a PULL and counts for the window. TCP only.
async fn run_fanout(
    role: Role,
    ep: Endpoint,
    peers: u32,
    duration: Duration,
    payload: &Bytes,
) -> Result<()> {
    let addr = match ep {
        Endpoint::Tcp(a) => a,
        Endpoint::Ipc(_) => bail!("monocoque fanout: tcp only for now"),
    };
    match role {
        Role::Pub => {
            let listener = TcpListener::bind(addr).await?;
            let mut fanout =
                PushFanOut::accept_workers(&listener, peers as usize, bulk_send_opts()).await?;
            let flush_every = 64u64 * u64::from(peers.max(1));
            let mut i = 0u64;
            loop {
                let _ = fanout.send_one(payload.clone()).await;
                i += 1;
                if i.is_multiple_of(flush_every) {
                    let _ = fanout.flush().await;
                }
            }
        }
        Role::Sub => {
            let stream = TcpStream::connect(addr).await?;
            let mut pull = PullSocket::from_tcp_with_options(stream, bulk_recv_opts()).await?;
            let mut buf: Vec<Bytes> = Vec::with_capacity(4);
            if !pull.recv_into(&mut buf).await.unwrap_or(false) {
                println!("THROUGHPUT 0 0.000001");
                return Ok(());
            }
            let mut count: u64 = 1;
            let t0 = Instant::now();
            let deadline = t0 + duration;
            while Instant::now() < deadline {
                match pull.recv_into(&mut buf).await {
                    Ok(true) => {
                        count += 1;
                        // Drain what already arrived without re-entering the
                        // reactor, the same batch-drain the throughput path uses.
                        while pull.try_recv_into(&mut buf).unwrap_or(false) {
                            count += 1;
                        }
                    }
                    _ => break,
                }
            }
            let elapsed = t0.elapsed().as_secs_f64();
            println!("THROUGHPUT {count} {elapsed:.6}");
        }
    }
    Ok(())
}

// ── fan-in (N PUSH -> 1 PULL) ────────────────────────────────────────────────

/// The sink binds a `PullFanIn` that accepts `peers` PUSH workers and counts the
/// merged stream for the window. Each producer connects a coalesced PUSH and
/// sends forever. TCP only.
async fn run_fanin(
    role: Role,
    ep: Endpoint,
    peers: u32,
    duration: Duration,
    payload: &Bytes,
) -> Result<()> {
    let addr = match ep {
        Endpoint::Tcp(a) => a,
        Endpoint::Ipc(_) => bail!("monocoque fanin: tcp only for now"),
    };
    match role {
        Role::Sub => {
            let listener = TcpListener::bind(addr).await?;
            let mut sink =
                PullFanIn::accept_workers(&listener, peers as usize, bulk_recv_opts()).await?;
            let mut count: u64 = if let Ok(Some(_)) = sink.recv().await {
                1
            } else {
                println!("THROUGHPUT 0 0.000001");
                return Ok(());
            };
            let t0 = Instant::now();
            let deadline = t0 + duration;
            'outer: while Instant::now() < deadline {
                match sink.recv().await {
                    Ok(Some(_)) => {
                        count += 1;
                        loop {
                            if Instant::now() >= deadline {
                                break 'outer;
                            }
                            match sink.try_recv() {
                                Ok(Some(_)) => count += 1,
                                _ => break,
                            }
                        }
                    }
                    _ => break,
                }
            }
            let elapsed = t0.elapsed().as_secs_f64();
            println!("THROUGHPUT {count} {elapsed:.6}");
        }
        Role::Pub => {
            let mut push = PushSocket::connect_with_options(addr, bulk_send_opts()).await?;
            let mut i = 0u64;
            loop {
                let _ = push.send_one(payload.clone()).await;
                i += 1;
                if i.is_multiple_of(64) {
                    let _ = push.flush().await;
                }
            }
        }
    }
    Ok(())
}
