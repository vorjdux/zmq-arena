//! The run archive as the reporter reads it.
//!
//! This is a deliberately partial view of the record schema: the fields the
//! charts plot, plus the stability block that says whether a cell may be
//! plotted at all. Unknown fields are ignored, so the orchestrator can add to a
//! record without breaking chart generation.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Archive {
    pub date: String,
    #[serde(default)]
    pub hardware: Hardware,
    pub records: Vec<Record>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Hardware {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Record {
    pub variant: String,
    pub kind: String,
    pub transport: String,
    pub payload_bytes: u64,
    #[serde(default)]
    pub peers: Option<u32>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub lib_version: Option<String>,
    /// Set only by bindings: the wrapper crate's own version. `lib_version` is
    /// then the engine underneath it, not this crate.
    #[serde(default)]
    pub binding_version: Option<String>,
    #[serde(default)]
    pub latency_ns: Option<Latency>,
    #[serde(default)]
    pub throughput: Option<Throughput>,
    #[serde(default)]
    pub stability: Option<Stability>,
}

#[derive(Debug, Deserialize)]
pub struct Latency {
    pub p50: f64,
    pub p99: f64,
}

#[derive(Debug, Deserialize)]
pub struct Throughput {
    pub msgs_per_s: f64,
    /// Megabits per second as the orchestrator records it.
    pub mbps: f64,
}

#[derive(Debug, Deserialize)]
pub struct Stability {
    /// Set when a cell's msgs/s is beaten by a larger payload in the same sweep,
    /// which is physically impossible on one path and marks the number as a
    /// measurement artifact.
    #[serde(default)]
    pub inverted: bool,
    #[serde(default)]
    pub stable: bool,
}

impl Record {
    /// A cell is plottable unless replication proved it wrong. Charts must not
    /// draw an inverted cell: it is a known-bad number, and a line through it
    /// reads as a real dip.
    pub fn plottable(&self) -> bool {
        !self.stability.as_ref().is_some_and(|s| s.inverted)
    }
}

/// payload size -> variant -> value.
pub type Series = BTreeMap<u64, BTreeMap<String, f64>>;

/// Selector for one chart's worth of cells.
pub struct Query<'a> {
    pub kind: &'a str,
    pub transport: &'a str,
    pub peers: Option<u32>,
}

impl Archive {
    /// Collect one metric into a payload-size keyed series, skipping cells that
    /// replication flagged as wrong and cells whose metric is missing or zero.
    pub fn series(&self, q: &Query<'_>, metric: impl Fn(&Record) -> Option<f64>) -> Series {
        let mut out = Series::new();
        for r in &self.records {
            if r.kind != q.kind || r.transport != q.transport || !r.plottable() {
                continue;
            }
            if let Some(p) = q.peers
                && r.peers != Some(p)
            {
                continue;
            }
            let Some(v) = metric(r).filter(|v| *v > 0.0) else {
                continue;
            };
            out.entry(r.payload_bytes)
                .or_default()
                .insert(r.variant.clone(), v);
        }
        out
    }

    /// Version string per variant, for the legend. Takes the first seen; a run
    /// measures one build of each engine.
    ///
    /// For a binding the wrapper's own version is the identity and the engine
    /// underneath is context, so it reads `0.5.0 (libzmq 4.3.4)`. Showing only
    /// `lib_version` there was actively misleading: it rendered as "tmq 4.3.4",
    /// which looks like a tmq release that does not exist. Native engines have
    /// no binding version and keep the plain `lib_version`.
    pub fn versions(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for r in &self.records {
            let label = match (&r.binding_version, &r.lib_version) {
                (Some(b), Some(l)) => {
                    let engine = r.engine.as_deref().unwrap_or("engine");
                    format!("{b} ({engine} {l})")
                }
                (Some(b), None) => b.clone(),
                (None, Some(l)) => l.clone(),
                (None, None) => continue,
            };
            out.entry(r.variant.clone()).or_insert(label);
        }
        out
    }

    /// How many of the plotted cells did not converge across replicates. Shown
    /// on the chart so a reader knows how much to trust the shape.
    pub fn unstable_count(&self, q: &Query<'_>) -> usize {
        self.records
            .iter()
            .filter(|r| r.kind == q.kind && r.transport == q.transport && r.plottable())
            .filter(|r| q.peers.is_none_or(|p| r.peers == Some(p)))
            .filter(|r| r.stability.as_ref().is_some_and(|s| !s.stable))
            .count()
    }
}

pub fn msgs(r: &Record) -> Option<f64> {
    r.throughput.as_ref().map(|t| t.msgs_per_s)
}

/// Mbit/s in the record, GB/s on the chart: the byte-rate axis is easier to
/// sanity-check against link and memory bandwidth than a bit rate.
pub fn gbps(r: &Record) -> Option<f64> {
    r.throughput.as_ref().map(|t| t.mbps / 8_000.0)
}

pub fn lat_p50_us(r: &Record) -> Option<f64> {
    r.latency_ns.as_ref().map(|l| l.p50 / 1000.0)
}

pub fn lat_p99_us(r: &Record) -> Option<f64> {
    r.latency_ns.as_ref().map(|l| l.p99 / 1000.0)
}
