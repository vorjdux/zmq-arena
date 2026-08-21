//! zmq-arena target wrapper: celerity (sans-IO ZMTP 3.1 with a Tokio driver).
//!
//! celerity's core is sans-IO; the crate's `io` module ships the Tokio sockets
//! this wrapper drives (`PubSocket`, `SubSocket`, `ReqSocket`, `RepSocket`).
//!
//! **Only latency and pub/sub run.** celerity 0.1.1 implements PUB/SUB and
//! REQ/REP and has no PUSH/PULL at all: there is no pipeline core in the crate,
//! so throughput, fan-out and fan-in have nothing to drive. They are rejected up
//! front rather than faked from a different pattern, and the matrix simply does
//! not schedule them for this target.
//!
//! Role and bind contract, set by the orchestrator (see targets/README.md):
//!   latency  REP(sub) binds,  REQ(pub) connects
//!   pubsub   PUB(pub) binds,  SUB(sub) connects   (--bind on pub)

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use bytes::Bytes;
use celerity::Multipart;
use celerity::io::{PubSocket, RepSocket, ReqSocket, SubSocket};
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    Pub,
    Sub,
}

#[derive(Parser, Debug)]
#[command(
    name = "celerity-target",
    version,
    about = "zmq-arena celerity wrapper"
)]
struct Cli {
    #[arg(long, value_enum)]
    role: Role,
    #[arg(long, default_value = "latency")]
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
    /// Measurement window for duration-based kinds (pubsub).
    #[arg(long, default_value_t = 0.0)]
    duration_secs: f64,
    /// Accepted and ignored: the crate's Tokio sockets expose no tuning knobs
    /// this wrapper would be justified in setting.
    #[arg(long = "knob")]
    knobs: Vec<String>,
}

/// One-line JSON classification the orchestrator captures into each record.
fn describe() -> String {
    format!(
        concat!(
            "{{\"engine\":\"celerity\",\"lib_version\":\"{}\",\"binding_version\":null,",
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
        "celerity-target: role={:?} kind={} transport={} endpoint={} payload={}B msgs={} warmup={}",
        cli.role,
        cli.kind,
        cli.transport,
        cli.endpoint,
        cli.payload_bytes,
        cli.messages,
        cli.warmup
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let payload = Bytes::from(vec![b'x'; cli.payload_bytes as usize]);
    let ep = cli.endpoint.clone();
    let duration = Duration::from_secs_f64(cli.duration_secs);
    match cli.kind.as_str() {
        "latency" => latency(cli.role, &ep, cli.messages, cli.warmup, &payload).await,
        "pubsub" => pubsub(cli.role, &ep, cli.peers.unwrap_or(1), duration, &payload).await,
        other => bail!(
            "celerity: kind '{other}' not supported. celerity 0.1.1 has no PUSH/PULL, \
             so throughput, fan-out and fan-in cannot be run against it."
        ),
    }
}

fn frame(payload: &Bytes) -> Multipart {
    vec![payload.clone()]
}

// ── latency (REQ/REP) ───────────────────────────────────────────────────────

async fn latency(role: Role, ep: &str, messages: u64, warmup: u64, payload: &Bytes) -> Result<()> {
    match role {
        Role::Sub => {
            let mut rep = RepSocket::bind(ep).await?;
            // Echo until the REQ side goes away, the normal end of the cell.
            while let Ok(msg) = rep.recv().await {
                if rep.reply(msg).await.is_err() {
                    break;
                }
            }
        }
        Role::Pub => {
            let req = ReqSocket::connect(ep).await?;
            for _ in 0..warmup {
                req.request(frame(payload)).await?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                // request() is send-then-receive in one call, which is exactly
                // the REQ/REP round trip being timed.
                req.request(frame(payload)).await?;
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

// ── pub/sub ─────────────────────────────────────────────────────────────────

async fn pubsub(
    role: Role,
    ep: &str,
    peers: u32,
    duration: Duration,
    payload: &Bytes,
) -> Result<()> {
    if !ep.starts_with("tcp://") {
        bail!("celerity pubsub: tcp only, got {ep}");
    }
    match role {
        Role::Pub => {
            let mut publisher = PubSocket::bind(ep).await?;
            // Wait for every subscriber before publishing: PUB drops messages
            // sent to peers that have not finished subscribing, so sending
            // early would measure the drop path rather than delivery.
            for _ in 0..peers.max(1) {
                publisher
                    .wait_for_subscriber(Duration::from_secs(30))
                    .await?;
            }
            while publisher.send(frame(payload)).await.is_ok() {}
        }
        Role::Sub => {
            let mut sub = SubSocket::connect(ep).await?;
            sub.subscribe(Bytes::new()).await?;
            if sub.recv().await.is_err() {
                println!("THROUGHPUT 0 0.000001");
                return Ok(());
            }
            let mut count: u64 = 1;
            let t0 = Instant::now();
            let deadline = t0 + duration;
            while Instant::now() < deadline {
                if sub.recv().await.is_err() {
                    break;
                }
                count += 1;
            }
            let secs = t0.elapsed().as_secs_f64().max(1e-9);
            println!("THROUGHPUT {count} {secs:.6}");
        }
    }
    Ok(())
}
