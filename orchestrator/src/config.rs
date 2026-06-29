//! Benchmark matrix schema.
//!
//! The matrix is the single source of truth for a grid run. The orchestrator
//! deserializes it, expands it into one [`MatrixEntry`] per (target x transport
//! x payload x pattern) cell, and executes each cell in isolated processes.
//!
//! Maintainers tune their implementation only through [`TargetSpec::knobs`].
//! The harness never reaches inside a target to set socket options; it forwards
//! the knob map verbatim as `--knob key=value` flags / environment variables so
//! optimization ownership stays with the library maintainer (Zero-Bias).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Network topology under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Loopback TCP inside a dedicated network namespace (`netns`) so the TCP
    /// state machine is exercised but isolated from host traffic.
    TcpNetns,
    /// UNIX domain socket. Measures user-to-kernel boundary traversal without
    /// the TCP state machine.
    Ipc,
    /// In-process across threads, single process. The one exception to the
    /// arena's process-isolation rule, included for parity with omq's inproc
    /// benchmarks. No netns/cgroup process pair is provisioned for these.
    Inproc,
}

/// Benchmark kind the cell measures. Mirrors the omq comparison harness.
/// Serializes to the dashboard archive tokens (throughput/latency/pubsub/
/// fanout/fanin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// PUSH/PULL one-to-one throughput.
    Throughput,
    /// REQ/REP round-trip latency.
    Latency,
    /// PUB/SUB throughput to `peers` subscribers.
    PubSub,
    /// 1 PUSH to N PULL (TCP only upstream).
    FanOut,
    /// N PUSH to 1 PULL (TCP only upstream).
    FanIn,
}

/// One implementation under test plus its maintainer-owned tuning knobs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetSpec {
    /// Stable identifier, e.g. `monocoque`, `libzmq_cpp`, `zeromq_rs`.
    pub id: String,
    /// Path to the compiled target binary (release profile).
    pub binary: PathBuf,
    /// Runtime variant selector, forwarded as `--variant`. One binary may expose
    /// several runtimes (e.g. omq-tokio current-thread vs multi-thread); each
    /// variant is measured as its own series. None means the binary's default.
    #[serde(default)]
    pub variant: Option<String>,
    /// Opaque knob map forwarded to the wrapper. Keys and value semantics are
    /// defined by the target maintainer, not the harness. Examples:
    /// `sndhwm`, `rcvhwm`, `tcp_nodelay`, `io_threads`, `batch_size`.
    #[serde(default)]
    pub knobs: BTreeMap<String, String>,
}

/// Fully expanded unit of work: one process pair, one measurement block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatrixEntry {
    pub target: TargetSpec,
    pub transport: Transport,
    pub kind: Kind,
    /// Subscriber/pusher count for pubsub/fanout/fanin; None for the others.
    #[serde(default)]
    pub peers: Option<u32>,
    /// Measurement window in seconds for the duration-based kinds (pubsub,
    /// fanout, fanin). Ignored by throughput/latency, which are message-counted.
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// Fixed payload size in bytes.
    pub payload_bytes: u32,
    /// Number of messages in the steady-state measurement block.
    pub messages: u64,
    /// Messages discarded before measurement to reach steady state.
    #[serde(default)]
    pub warmup_messages: u64,
}

/// Resource containment applied per target process via cgroup v2.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Isolation {
    /// `cpuset.cpus` value, e.g. "2" or "2-3". Pins the target to dedicated
    /// cores to remove scheduler migration noise from the tail.
    pub cpuset_cpus: String,
    /// `cpuset.mems` NUMA node binding.
    #[serde(default)]
    pub cpuset_mems: Option<String>,
    /// `memory.max` byte cap. Bounds allocation and surfaces leaks across the
    /// run.
    pub memory_max_bytes: u64,
}

/// Top-level run definition loaded from `matrix.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunConfig {
    pub isolation: Isolation,
    pub entries: Vec<MatrixEntry>,
}

impl RunConfig {
    /// Parse a matrix file. No I/O side effects beyond the read.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let cfg: RunConfig = serde_json::from_str(&raw)?;
        Ok(cfg)
    }
}
