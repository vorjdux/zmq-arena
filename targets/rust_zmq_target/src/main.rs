//! zmq-arena target wrapper: rust-zmq (the `zmq` crate, a safe binding over libzmq).
//!
//! Same engine as libzmq_cpp_target (the C ZeroMQ core); the difference is the
//! path to it, a Rust FFI binding instead of the C++ C API. Running both isolates
//! the binding overhead. The sockets are blocking and each role runs as its own
//! process, so libzmq handles peer fan-out and fan-in on the bound socket and all
//! five kinds are supported.
//!
//! Role and bind contract, set by the orchestrator (see targets/README.md):
//!   throughput  PULL(sub) binds,  PUSH(pub) connects
//!   latency     REP(sub) binds,   REQ(pub) connects
//!   pubsub      PUB(pub) binds,   SUB(sub) connect    (--bind on pub)
//!   fanout      PUSH(pub) binds,  PULL(sub) connect   (--bind on pub)
//!   fanin       PULL(sub) binds,  PUSH(pub) connect   (--bind on sub)
//!
//! Knobs: io_threads (on the context), sndhwm and rcvhwm (on the socket), matching
//! the libzmq C++ target. Duration-based kinds are TCP only, like the orchestrator.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(name = "rust-zmq-target", version, about = "zmq-arena rust-zmq wrapper")]
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let knobs: BTreeMap<String, String> = cli.knobs.iter().cloned().collect();
    eprintln!(
        "rust-zmq-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={} variant={}",
        cli.role, cli.kind, cli.transport, cli.endpoint, cli.payload_bytes, cli.messages, cli.warmup, cli.variant
    );

    let payload = vec![b'x'; cli.payload_bytes as usize];
    let duration = Duration::from_secs_f64(cli.duration_secs);
    match cli.kind.as_str() {
        "throughput" => run_throughput(&cli, &knobs, cli.messages + cli.warmup, &payload),
        "latency" => run_latency(&cli, &knobs, cli.messages, cli.warmup, &payload),
        "pubsub" => run_pubsub(&cli, &knobs, duration, &payload),
        "fanout" => run_fanout(&cli, &knobs, duration, &payload),
        "fanin" => run_fanin(&cli, &knobs, duration, &payload),
        other => bail!("rust-zmq: kind '{other}' not implemented"),
    }
}

// ── context and socket option knobs ─────────────────────────────────────────

fn make_context(knobs: &BTreeMap<String, String>) -> Result<zmq::Context> {
    let ctx = zmq::Context::new();
    if let Some(n) = knobs.get("io_threads").and_then(|v| v.parse::<i32>().ok()) {
        ctx.set_io_threads(n)?;
    }
    Ok(ctx)
}

fn apply_hwm(sock: &zmq::Socket, knobs: &BTreeMap<String, String>) -> Result<()> {
    if let Some(v) = knobs.get("sndhwm").and_then(|v| v.parse::<i32>().ok()) {
        sock.set_sndhwm(v)?;
    }
    if let Some(v) = knobs.get("rcvhwm").and_then(|v| v.parse::<i32>().ok()) {
        sock.set_rcvhwm(v)?;
    }
    Ok(())
}

// ── throughput (PUSH/PULL) ──────────────────────────────────────────────────

/// PULL binds and receives exactly `total`; PUSH connects and sends `total`.
/// PUSH/PULL is lossless under HWM back-pressure, so the counts match.
fn run_throughput(cli: &Cli, knobs: &BTreeMap<String, String>, total: u64, payload: &[u8]) -> Result<()> {
    let ctx = make_context(knobs)?;
    match cli.role {
        Role::Sub => {
            let sock = ctx.socket(zmq::PULL)?;
            apply_hwm(&sock, knobs)?;
            sock.bind(&cli.endpoint).with_context(|| format!("bind {}", cli.endpoint))?;
            let mut buf = vec![0u8; payload.len().max(1)];
            let mut count = 0u64;
            while count < total {
                sock.recv_into(&mut buf, 0)?;
                count += 1;
            }
        }
        Role::Pub => {
            let sock = ctx.socket(zmq::PUSH)?;
            apply_hwm(&sock, knobs)?;
            sock.connect(&cli.endpoint).with_context(|| format!("connect {}", cli.endpoint))?;
            for _ in 0..total {
                sock.send(payload, 0)?;
            }
        }
    }
    Ok(())
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

/// REP binds and echoes until killed. REQ connects, runs `warmup` untimed
/// round-trips, then times `messages` and prints the quantiles.
fn run_latency(
    cli: &Cli,
    knobs: &BTreeMap<String, String>,
    messages: u64,
    warmup: u64,
    payload: &[u8],
) -> Result<()> {
    let ctx = make_context(knobs)?;
    match cli.role {
        Role::Sub => {
            let sock = ctx.socket(zmq::REP)?;
            sock.bind(&cli.endpoint).with_context(|| format!("bind {}", cli.endpoint))?;
            let mut buf = vec![0u8; payload.len().max(1)];
            loop {
                match sock.recv_into(&mut buf, 0) {
                    Ok(n) => sock.send(&buf[..n.min(buf.len())], 0)?,
                    Err(_) => break, // context terminated
                }
            }
        }
        Role::Pub => {
            let sock = ctx.socket(zmq::REQ)?;
            sock.connect(&cli.endpoint).with_context(|| format!("connect {}", cli.endpoint))?;
            let mut buf = vec![0u8; payload.len().max(1)];
            for _ in 0..warmup {
                sock.send(payload, 0)?;
                sock.recv_into(&mut buf, 0)?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                sock.send(payload, 0)?;
                sock.recv_into(&mut buf, 0)?;
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
fn run_pubsub(cli: &Cli, knobs: &BTreeMap<String, String>, duration: Duration, payload: &[u8]) -> Result<()> {
    require_tcp("pubsub", &cli.endpoint)?;
    let ctx = make_context(knobs)?;
    match cli.role {
        Role::Pub => {
            let sock = ctx.socket(zmq::PUB)?;
            apply_hwm(&sock, knobs)?;
            sock.bind(&cli.endpoint).with_context(|| format!("bind {}", cli.endpoint))?;
            loop {
                if sock.send(payload, 0).is_err() {
                    break;
                }
            }
        }
        Role::Sub => {
            let sock = ctx.socket(zmq::SUB)?;
            apply_hwm(&sock, knobs)?;
            sock.connect(&cli.endpoint).with_context(|| format!("connect {}", cli.endpoint))?;
            sock.set_subscribe(b"")?;
            count_window(&sock, duration, payload.len())?;
        }
    }
    Ok(())
}

// ── fan-out (1 PUSH -> N PULL) ───────────────────────────────────────────────

/// PUSH binds and load-balances forever; libzmq round-robins across the
/// connected PULLs. Each PULL connects and counts its slice. TCP only.
fn run_fanout(cli: &Cli, knobs: &BTreeMap<String, String>, duration: Duration, payload: &[u8]) -> Result<()> {
    require_tcp("fanout", &cli.endpoint)?;
    let ctx = make_context(knobs)?;
    match cli.role {
        Role::Pub => {
            let sock = ctx.socket(zmq::PUSH)?;
            apply_hwm(&sock, knobs)?;
            sock.bind(&cli.endpoint).with_context(|| format!("bind {}", cli.endpoint))?;
            loop {
                if sock.send(payload, 0).is_err() {
                    break;
                }
            }
        }
        Role::Sub => {
            let sock = ctx.socket(zmq::PULL)?;
            apply_hwm(&sock, knobs)?;
            sock.connect(&cli.endpoint).with_context(|| format!("connect {}", cli.endpoint))?;
            count_window(&sock, duration, payload.len())?;
        }
    }
    Ok(())
}

// ── fan-in (N PUSH -> 1 PULL) ────────────────────────────────────────────────

/// PULL binds and fair-queues the merged stream from N PUSHers, counting for the
/// window. Each PUSH connects and sends forever. TCP only.
fn run_fanin(cli: &Cli, knobs: &BTreeMap<String, String>, duration: Duration, payload: &[u8]) -> Result<()> {
    require_tcp("fanin", &cli.endpoint)?;
    let ctx = make_context(knobs)?;
    match cli.role {
        Role::Sub => {
            let sock = ctx.socket(zmq::PULL)?;
            apply_hwm(&sock, knobs)?;
            sock.bind(&cli.endpoint).with_context(|| format!("bind {}", cli.endpoint))?;
            count_window(&sock, duration, payload.len())?;
        }
        Role::Pub => {
            let sock = ctx.socket(zmq::PUSH)?;
            apply_hwm(&sock, knobs)?;
            sock.connect(&cli.endpoint).with_context(|| format!("connect {}", cli.endpoint))?;
            loop {
                if sock.send(payload, 0).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

// ── shared helpers ──────────────────────────────────────────────────────────

/// Count received messages over `duration`, starting the clock on the first
/// message to skip the connect/accept ramp, then print `THROUGHPUT count secs`.
/// A 200 ms recv timeout lets the loop observe the deadline if the stream stalls;
/// libzmq returns EAGAIN on timeout.
fn count_window(sock: &zmq::Socket, duration: Duration, payload_len: usize) -> Result<()> {
    sock.set_rcvtimeo(200)?;
    let mut buf = vec![0u8; payload_len.max(1)];
    loop {
        match sock.recv_into(&mut buf, 0) {
            Ok(_) => break,
            Err(zmq::Error::EAGAIN) => continue, // still waiting for the first message
            Err(e) => {
                println!("THROUGHPUT 0 0.000001");
                return Err(e.into());
            }
        }
    }
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        match sock.recv_into(&mut buf, 0) {
            Ok(_) => count += 1,
            Err(zmq::Error::EAGAIN) => {} // timeout tick, re-check the deadline
            Err(_) => break,
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    println!("THROUGHPUT {count} {elapsed:.6}");
    Ok(())
}

fn require_tcp(kind: &str, ep: &str) -> Result<()> {
    if !ep.starts_with("tcp://") {
        bail!("rust-zmq {kind}: tcp only (got {ep})");
    }
    Ok(())
}
