//! The `blocking` variant: omq's synchronous socket API over library-owned IO
//! threads.
//!
//! This is a third execution model, not a third flavour of the tokio one. The
//! two async variants embed omq in a runtime this wrapper builds and drives; the
//! blocking variant hands that job to omq itself. `Context::new()` spawns an IO
//! thread pool, each thread running its own current-thread runtime with
//! connections pinned to a thread for life, and the caller then talks to sockets
//! through plain synchronous calls with no runtime of its own.
//!
//! It is worth measuring separately because it is the model libzmq uses: a sync
//! API in front of library-owned IO threads. Comparing it against the async
//! variants isolates what the caller's runtime costs, and comparing it against
//! libzmq compares two implementations of the same idea.
//!
//! The loops below mirror the async ones in `main.rs` exactly, minus `.await`.
//! Any change to measurement policy (warmup handling, when the clock starts,
//! what gets printed) has to land in both or the two variants stop being
//! comparable.

use anyhow::Result;
use bytes::Bytes;
use omq_tokio::blocking::Socket;
use omq_tokio::{Context, ContextConfig, Endpoint, Message, SocketType};
use std::time::{Duration, Instant};

use crate::{Role, bench_opts, print_latency, sender_opts};

/// One IO thread, matching the single-threaded async variants and libzmq's
/// `io_threads = 1` knob in the matrix, so the comparison is not decided by one
/// engine being handed more threads than the others.
fn context() -> Context {
    Context::with_config(ContextConfig { io_threads: 1 })
}

pub fn run(
    role: Role,
    kind: &str,
    ep: Endpoint,
    messages: u64,
    warmup: u64,
    duration: Duration,
    payload: &Bytes,
) -> Result<()> {
    match kind {
        "throughput" => throughput(role, ep, messages, warmup, payload),
        "latency" => latency(role, ep, messages, warmup, payload),
        "pubsub" => pubsub(role, ep, duration, payload),
        "fanout" => fanout(role, ep, duration, payload),
        "fanin" => fanin(role, ep, duration, payload),
        other => anyhow::bail!("omq blocking: kind '{other}' not implemented"),
    }
}

fn throughput(role: Role, ep: Endpoint, messages: u64, warmup: u64, payload: &Bytes) -> Result<()> {
    let ctx = context();
    match role {
        Role::Sub => {
            let pull = ctx.blocking_socket(SocketType::Pull, bench_opts(payload.len()));
            pull.bind(ep)?;
            for _ in 0..warmup {
                pull.recv()?;
            }
            let t0 = Instant::now();
            let mut measured = 0u64;
            while measured < messages {
                pull.recv()?;
                measured += 1;
            }
            let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
            println!("THROUGHPUT {measured} {elapsed:.6}");
        }
        Role::Pub => {
            let push = ctx.blocking_socket(SocketType::Push, bench_opts(payload.len()));
            push.connect(ep)?;
            // Send until killed, for the same reason the async variant does: a
            // fixed count can exit with messages still queued in the socket, so
            // the consumer's count is the only stop condition.
            while push.send(Message::single(payload.clone())).is_ok() {}
        }
    }
    Ok(())
}

fn latency(role: Role, ep: Endpoint, messages: u64, warmup: u64, payload: &Bytes) -> Result<()> {
    let ctx = context();
    match role {
        Role::Sub => {
            let rep = ctx.blocking_socket(SocketType::Rep, bench_opts(payload.len()));
            rep.bind(ep)?;
            while let Ok(msg) = rep.recv() {
                if rep.send(msg).is_err() {
                    break;
                }
            }
        }
        Role::Pub => {
            let req = ctx.blocking_socket(SocketType::Req, bench_opts(payload.len()));
            req.connect(ep)?;
            for _ in 0..warmup {
                req.send(Message::single(payload.clone()))?;
                req.recv()?;
            }
            let mut rtts: Vec<u64> = Vec::with_capacity(messages as usize);
            for _ in 0..messages {
                let t = Instant::now();
                req.send(Message::single(payload.clone()))?;
                req.recv()?;
                rtts.push(t.elapsed().as_nanos() as u64);
            }
            print_latency(&mut rtts);
        }
    }
    Ok(())
}

fn pubsub(role: Role, ep: Endpoint, duration: Duration, payload: &Bytes) -> Result<()> {
    let ctx = context();
    match role {
        Role::Pub => {
            let publisher = ctx.blocking_socket(SocketType::Pub, sender_opts(payload.len()));
            publisher.bind(ep)?;
            while publisher.send(Message::single(payload.clone())).is_ok() {}
        }
        Role::Sub => {
            let sub = ctx.blocking_socket(SocketType::Sub, bench_opts(payload.len()));
            sub.connect(ep)?;
            sub.subscribe(Bytes::new())?;
            count_window(&sub, duration);
        }
    }
    Ok(())
}

fn fanout(role: Role, ep: Endpoint, duration: Duration, payload: &Bytes) -> Result<()> {
    let ctx = context();
    match role {
        Role::Pub => {
            let push = ctx.blocking_socket(SocketType::Push, bench_opts(payload.len()));
            push.bind(ep)?;
            while push.send(Message::single(payload.clone())).is_ok() {}
        }
        Role::Sub => {
            let pull = ctx.blocking_socket(SocketType::Pull, bench_opts(payload.len()));
            pull.connect(ep)?;
            count_window(&pull, duration);
        }
    }
    Ok(())
}

fn fanin(role: Role, ep: Endpoint, duration: Duration, payload: &Bytes) -> Result<()> {
    let ctx = context();
    match role {
        Role::Sub => {
            let pull = ctx.blocking_socket(SocketType::Pull, bench_opts(payload.len()));
            pull.bind(ep)?;
            count_window(&pull, duration);
        }
        Role::Pub => {
            let push = ctx.blocking_socket(SocketType::Push, bench_opts(payload.len()));
            push.connect(ep)?;
            while push.send(Message::single(payload.clone())).is_ok() {}
        }
    }
    Ok(())
}

/// Sync twin of `main.rs::count_window`: clock starts on the first message so
/// the connect ramp is excluded, bursts are drained with `try_recv`, and the
/// same `THROUGHPUT count secs` line is printed.
fn count_window(sock: &Socket, duration: Duration) {
    if sock.recv().is_err() {
        println!("THROUGHPUT 0 0.000001");
        return;
    }
    let mut count: u64 = 1;
    let t0 = Instant::now();
    let deadline = t0 + duration;
    while Instant::now() < deadline {
        if sock.recv().is_err() {
            break;
        }
        count += 1;
        while Instant::now() < deadline && sock.try_recv().is_ok() {
            count += 1;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
    println!("THROUGHPUT {count} {elapsed:.6}");
}
