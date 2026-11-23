//! zmq-arena target wrapper: monocoque (monocoque-rs 1.0, ZMTP 3.1).
//!
//! All five kinds over TCP and IPC, and the two count-based kinds over inproc.
//! 1.0 has one message type and sockets that bind or connect any number of
//! endpoints themselves, so the fan patterns are plain PUSH and PULL sockets
//! and every kind runs on every transport the socket accepts.
//!
//! Tuning follows the engine's documented levers (`docs/performance.md` and the
//! `coalesced_push_pull` example in the monocoque repository) and nothing else:
//!   - bulk senders (throughput, fan-out, fan-in, PUB) turn write coalescing on
//!     and let the buffer write itself out at its threshold;
//!   - REQ/REP leaves coalescing off, so a request leaves as soon as it is sent;
//!   - everything else is the engine's default, each one checked against the
//!     alternatives (see the socket setup section below);
//!   - every receive borrows the socket's receive slot, so counting a message
//!     allocates nothing, and every send passes a refcounted clone of one
//!     payload (PUB publishes it by reference).
//!
//! The orchestrator spawns the consumer (`--role sub`, binds) first, then the
//! producer (`--role pub`, connects). monocoque's runtime is a compile-time
//! choice, so this wrapper builds once per runtime the engine ships and reports
//! each as its own variant: `compio` (`io_uring`, the default), `tokio` (epoll)
//! and `smol` (epoll). The socket loops are identical across all three, so the
//! only thing that differs between the series is the IO model.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{
    Msg, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket, SocketOptions, SubSocket,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug, Clone)]
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
    /// `key=value` knobs. None are read: a 1.0 socket is one multiplexer on the
    /// thread that owns it, so there is no worker or IO-thread count to set.
    /// Unknown keys are ignored so a shared matrix entry does not break this
    /// target.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record. The
/// runtime is a compile-time choice (`runtime-compio` / `runtime-tokio` /
/// `runtime-smol`), so `describe` reports whichever backend this binary was
/// built with. All three are single-threaded: a socket does its IO on the
/// runtime thread that owns it. The engine version is read from Cargo.lock at
/// build time (build.rs).
fn describe() -> String {
    // compio drives io_uring; tokio and smol both drive epoll on Linux, through
    // mio and polling respectively.
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
    if cli.transport == "inproc" {
        return run_inproc(&cli);
    }
    run_role(&cli, cli.role, None)
}

/// Inproc runs in one process: the orchestrator spawns only the measured end
/// (the consumer for throughput, the REQ client for latency), and the other end
/// runs here on a second thread with its own runtime, so messages cross threads
/// as they would between two sockets of one application. The binding end comes
/// up first, since an inproc connect needs a bound peer.
fn run_inproc(cli: &Cli) -> Result<()> {
    let measured = cli.role;
    let other = if measured == Role::Pub {
        Role::Sub
    } else {
        Role::Pub
    };
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread_cli = cli.clone();
    if other == Role::Sub {
        std::thread::spawn(move || run_role(&thread_cli, other, Some(ready_tx)));
        ready_rx.recv()?;
        run_role(cli, measured, None)
    } else {
        let main_cli = cli.clone();
        let measured = std::thread::spawn(move || run_role(&main_cli, measured, Some(ready_tx)));
        ready_rx.recv()?;
        std::thread::spawn(move || run_role(&thread_cli, other, None));
        measured
            .join()
            .map_err(|_| anyhow::anyhow!("measured thread panicked"))?
    }
}

/// Run one end of the cell on this thread. `ready` is signalled once a binding
/// end has bound.
fn run_role(cli: &Cli, role: Role, ready: Option<std::sync::mpsc::Sender<()>>) -> Result<()> {
    let run = Run {
        role,
        endpoint: cli.endpoint.clone(),
        messages: cli.messages,
        warmup: cli.warmup,
        peers: cli.peers.unwrap_or(1).max(1) as usize,
        duration: Duration::from_secs_f64(cli.duration_secs),
        // One payload for the whole run. A send clones it, which copies nothing:
        // up to 64 B it lives inline in the `Msg`, above that it is one
        // refcounted buffer. A `'static` payload, which also drops the
        // refcount, measured no faster.
        payload: Msg::from(vec![b'x'; cli.payload_bytes as usize]),
        ready,
    };
    let kind = cli.kind.clone();
    // The runtime-agnostic driver: compio's io_uring runtime, tokio's
    // current-thread runtime or smol's local executor, per the compiled feature.
    // Its spin before sleeping stays at the engine's default. Turning it off
    // cost REQ/REP two microseconds of p50 (5.4 to 7.6 at 64 B on compio) and
    // moved the bulk kinds in both directions.
    LocalRuntime::new()?.block_on(async move {
        match kind.as_str() {
            "throughput" => run.throughput().await,
            "latency" => run.latency().await,
            "pubsub" => run.pubsub().await,
            "fanout" => run.fanout().await,
            "fanin" => run.fanin().await,
            other => bail!("monocoque: kind '{other}' not implemented"),
        }
    })
}

// ── per-workload socket setup ───────────────────────────────────────────────
//
// Each value below was measured with this wrapper against monocoque-rs 1.0.0
// on an i7-1355U, both ends in cpuset 0-3 on host loopback, median of three
// to five runs per runtime. That is a dev host with two physical cores in the
// cpuset: the figures justify the choices and are not results.

/// Sender profile for the bulk streams (PUSH in throughput, fan-out and fan-in,
/// and PUB): write coalescing on, and nothing else.
///
/// Coalescing is the engine's documented throughput setting and its largest
/// lever. A coalescing PUB delivered 25 to 30 M msg/s to 32 subscribers at
/// 64 B; an eager one, which pays one write per subscriber per message,
/// 0.2 to 0.3 M.
///
/// The threshold stays at its 64 KiB default: 16 KiB and 32 KiB were slower
/// from 64 B up (7.9 M msg/s at 1 KiB on compio against 3.8 M and 5.3 M), and
/// 128 KiB was no faster.
///
/// The senders do not call `flush()` on a count. The buffer is written out
/// whenever it reaches the threshold, and flushing every 64 messages, as this
/// target did for 0.4, cuts a 64 KiB write down to the size of the batch: on
/// PUSH/PULL it halved the rate at 16 B and 64 B (15 M msg/s against 30 M) and
/// on fan-out it cost a factor of five (4 M against 21 M at 64 B).
fn bulk_send_opts() -> SocketOptions {
    SocketOptions::default().with_write_coalescing(true)
}

/// Receiver profile for the bulk streams: the defaults.
///
/// The read buffer starts at 32 KiB and grows to 64 KiB by itself while reads
/// keep filling it, so asking for 4 KiB, 16 KiB or the full 64 KiB up front
/// moved no payload size beyond run-to-run spread. The HWM stays at 1000 in
/// both directions, as libzmq's, everywhere but fan-in.
fn bulk_recv_opts() -> SocketOptions {
    SocketOptions::default()
}

/// Queue bound for both ends of fan-in, in messages per peer.
///
/// Fan-in is the one kind where several producers feed a consumer that cannot
/// keep up with them, so the queues are always full, and at the default of
/// 1000 that is megabytes of messages copied ahead of a consumer with no room
/// for them. With 64 on both ends, against 1000, at 4 producers:
///
/// | payload | compio      | tokio       | smol        |
/// |---------|-------------|-------------|-------------|
/// | 1 KiB   | 4.39 → 4.54 | 4.83 → 7.05 | 4.11 → 4.99 |
/// | 4 KiB   | 1.20 → 1.50 | 1.04 → 2.10 | 1.01 → 1.54 |
/// | 16 KiB  | 0.41 → 0.39 | 0.39 → 0.60 | 0.31 → 0.36 |
///
/// (M msg/s.) At 16 B to 256 B the two settings were within spread on compio
/// and smol; on tokio the cell is bimodal at 16 B and 64 B with either
/// setting (18 or 24 M msg/s), so the bound is not shown to be free there. It
/// has to be on both ends: on the consumer alone it moved nothing on compio.
/// Lowering the send HWM on the other kinds traded one payload size against
/// another (fan-out on compio gained at 4 KiB and lost at 1 KiB), so they keep
/// the default.
const FAN_IN_HWM: usize = 64;

/// Body size from which the multi-peer senders (PUB, and PUSH in fan-out) hand
/// the payload to the kernel as an iovec instead of copying it into the send
/// buffer. The engine's default is 16 KiB.
///
/// A PUB copies a smaller body once per subscriber, so at 4 KiB and 32
/// subscribers the copies were most of the publisher's work. With 4 KiB against
/// the default, at a 4 KiB payload:
///
/// | kind      | compio      | tokio       | smol        |
/// |-----------|-------------|-------------|-------------|
/// | PUB/SUB   | 1.06 → 1.39 | 1.22 → 1.67 | 1.15 → 1.44 |
/// | fan-out   | 1.21 → 1.60 | 1.73 → 1.66 | 1.18 → 1.19 |
/// | PUSH/PULL | 2.22 → 2.08 | 2.06 → 2.04 | 1.99 → 1.91 |
///
/// (M msg/s.) PUB at 64 B, 1 KiB and 16 KiB was within spread of the default
/// on all three runtimes, and 2 KiB did no better than 4 KiB. At 1 KiB a
/// 1 KiB threshold cost tokio's PUB a third (4.9 M against 3.2 M), so it goes
/// no lower. The single-peer PUSH keeps the default because compio lost 7%
/// there, and fan-in keeps it because nothing moved.
const MULTI_PEER_VECTORED_BYTES: usize = 4096;

/// [`bulk_send_opts`] for a sender with several peers.
fn multi_peer_send_opts() -> SocketOptions {
    bulk_send_opts().with_vectored_write_threshold(MULTI_PEER_VECTORED_BYTES)
}

/// Request/reply profile. Coalescing is off by default and stated here for the
/// intent: a request has to leave as soon as it is sent. A 4 KiB read buffer,
/// the "low latency" setting, left p50 where it was (5.4 against 5.5 µs at
/// 64 B on compio), so the read buffer stays at its default too.
fn latency_opts() -> SocketOptions {
    SocketOptions::default().with_write_coalescing(false)
}

/// Count messages received on `$sock` for `$window`, starting the clock at the
/// first one so the connection ramp is not in the rate, and print
/// `THROUGHPUT <n> <secs>`. A macro because each socket type has its own
/// borrowed `recv`.
macro_rules! count_window {
    ($window:expr, $sock:ident) => {{
        if $sock.recv().await.is_err() {
            println!("THROUGHPUT 0 0.000001");
            return Ok(());
        }
        let mut count = 1u64;
        let t0 = Instant::now();
        let deadline = t0 + $window;
        while Instant::now() < deadline {
            if $sock.recv().await.is_err() {
                break;
            }
            count += 1;
        }
        println!("THROUGHPUT {count} {:.6}", t0.elapsed().as_secs_f64());
        Ok(())
    }};
}

struct Run {
    role: Role,
    endpoint: String,
    messages: u64,
    warmup: u64,
    peers: usize,
    duration: Duration,
    payload: Msg,
    ready: Option<std::sync::mpsc::Sender<()>>,
}

impl Run {
    /// Tell an inproc peer thread that this end has bound.
    fn bound(&self) {
        if let Some(tx) = &self.ready {
            let _ = tx.send(());
        }
    }

    /// PUSH sends `warmup + messages`; PULL drains the warmup untimed, then
    /// times the `messages` block and prints `THROUGHPUT <n> <secs>`. Timing
    /// the measured block here (not the orchestrator's wall clock) keeps
    /// process spawn, the handshake and the warmup transfer out of the rate.
    async fn throughput(self) -> Result<()> {
        let total = self.warmup + self.messages;
        if self.role == Role::Pub {
            let mut push = PushSocket::with_options(bulk_send_opts());
            push.connect(&self.endpoint).await?;
            for _ in 0..total {
                push.send(self.payload.clone()).await?;
            }
            // The burst is over: write out what did not reach the threshold.
            // `close` then waits for the queued bytes to leave.
            push.flush().await?;
            push.close().await?;
            return Ok(());
        }
        let mut pull = PullSocket::with_options(bulk_recv_opts());
        pull.bind(&self.endpoint).await?;
        self.bound();
        for _ in 0..self.warmup {
            pull.recv().await?;
        }
        let t0 = Instant::now();
        for _ in 0..self.messages {
            pull.recv().await?;
        }
        let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
        println!("THROUGHPUT {} {elapsed:.6}", self.messages);
        Ok(())
    }

    /// REP echoes until the peer goes away; REQ times each round trip and
    /// prints `LATENCY <n> <min> <p50> <p90> <p99> <p99.9> <max>` in ns.
    async fn latency(self) -> Result<()> {
        if self.role == Role::Sub {
            let mut rep = RepSocket::with_options(latency_opts());
            rep.bind(&self.endpoint).await?;
            self.bound();
            loop {
                let Ok(msg) = rep.recv().await else {
                    return Ok(());
                };
                let msg = msg.take();
                rep.send(msg).await?;
            }
        }
        let mut req = ReqSocket::with_options(latency_opts());
        req.connect(&self.endpoint).await?;
        for _ in 0..self.warmup {
            req.send(self.payload.clone()).await?;
            req.recv().await?;
        }
        let mut rtts: Vec<u64> = Vec::with_capacity(self.messages as usize);
        for _ in 0..self.messages {
            let t = Instant::now();
            req.send(self.payload.clone()).await?;
            req.recv().await?;
            rtts.push(t.elapsed().as_nanos() as u64);
        }
        print_latency(&mut rtts);
        Ok(())
    }

    /// PUB binds, waits for `peers` subscribers, then publishes until killed.
    /// Each SUB counts for `duration` from its first message.
    async fn pubsub(self) -> Result<()> {
        if self.role == Role::Pub {
            let mut publisher = PubSocket::with_options(multi_peer_send_opts());
            publisher.bind(&self.endpoint).await?;
            // Publishing to subscribers whose subscription has not landed yet
            // would only drop messages, so wait until every one is counted.
            while publisher.send_ref(&self.payload).await? < self.peers {
                monocoque::rt::sleep(Duration::from_millis(1)).await;
            }
            loop {
                // PUB never waits on a send. When a subscriber's queue is full
                // the message reaches fewer peers than are connected, and
                // waiting for the queued bytes to be written is what lets that
                // subscriber catch up. The count is the live one, not `peers`:
                // subscribers leave as their windows end, and against the
                // starting count every send after the first departure would
                // look short and flush.
                if publisher.send_ref(&self.payload).await? < publisher.peer_count() {
                    publisher.flush().await?;
                }
            }
        }
        let mut sub = SubSocket::with_options(bulk_recv_opts());
        sub.subscribe(b"").await?;
        sub.connect(&self.endpoint).await?;
        count_window!(self.duration, sub)
    }

    /// One PUSH binds and round-robins to `peers` PULLs until killed; each PULL
    /// counts its share for the window.
    async fn fanout(self) -> Result<()> {
        if self.role == Role::Pub {
            let mut push = PushSocket::with_options(multi_peer_send_opts());
            push.bind(&self.endpoint).await?;
            while push.peer_count() < self.peers {
                monocoque::rt::sleep(Duration::from_millis(1)).await;
            }
            self.send_forever(push).await
        } else {
            let mut pull = PullSocket::with_options(bulk_recv_opts());
            pull.connect(&self.endpoint).await?;
            count_window!(self.duration, pull)
        }
    }

    /// `peers` PUSH sockets connect and send until killed; the bound PULL
    /// counts the merged stream for the window.
    async fn fanin(self) -> Result<()> {
        if self.role == Role::Pub {
            let mut push = PushSocket::with_options(bulk_send_opts().with_send_hwm(FAN_IN_HWM));
            push.connect(&self.endpoint).await?;
            self.send_forever(push).await
        } else {
            let mut pull = PullSocket::with_options(bulk_recv_opts().with_recv_hwm(FAN_IN_HWM));
            pull.bind(&self.endpoint).await?;
            count_window!(self.duration, pull)
        }
    }

    async fn send_forever(&self, mut push: PushSocket) -> Result<()> {
        loop {
            push.send(self.payload.clone()).await?;
        }
    }
}

fn print_latency(rtts: &mut [u64]) {
    if rtts.is_empty() {
        println!("LATENCY 0 0 0 0 0 0 0");
        return;
    }
    rtts.sort_unstable();
    let q = |p: f64| rtts[((rtts.len() as f64 * p) as usize).min(rtts.len() - 1)];
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
