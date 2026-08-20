//! The chart set.
//!
//! Two tiers, matching the matrix in `scripts/gen_matrix.py`:
//!
//!   headline_*  tcp, the three patterns every implementation here implements.
//!   extended_*  ipc, fan-out and fan-in.
//!
//! The tiers split by pattern, never by library: every chart draws every
//! implementation that produced cells for it. A library is absent from a chart
//! only when it did not, or could not, run that pattern.

use std::path::Path;

use anyhow::Result;
use plotters::prelude::*;

use crate::model::{self, Archive, Query};
use crate::theme::{self, IMPLS, Impl};

const W: u32 = 950;
const H: u32 = 470;
const HEADER_H: u32 = 62;
/// Minimum legend block; the real height depends on how many series wrap into
/// how many rows, and is computed per chart.
const LEGEND_MIN_H: u32 = 38;
const FOOTNOTE_H: u32 = 18;

/// Payload sizes present in the archive, ascending. Read from the data rather
/// than hardcoded so a `--sizes` run charts what it actually measured.
fn sizes_of(series: &[&model::Series]) -> Vec<u64> {
    let mut v: Vec<u64> = series
        .iter()
        .flat_map(|s| s.keys().copied())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    v.sort_unstable();
    v
}

fn provenance(a: &Archive) -> String {
    let hw = a.hardware.cpu.as_deref().unwrap_or("unknown host");
    let mut line = format!("run {} - {hw}", a.date);
    if let Some(note) = a.hardware.note.as_deref() {
        line.push_str(" - ");
        line.push_str(note);
    }
    line
}

fn footnote(unstable: usize) -> String {
    if unstable == 0 {
        "Cells flagged inverted by replication are omitted.".into()
    } else {
        format!(
            "{unstable} plotted cell(s) did not converge across replicates; \
             read the shape, not the exact value. Inverted cells are omitted."
        )
    }
}

/// Everything a chart says about itself: what it is, what it measures, where the
/// numbers came from, and what to distrust about them.
struct Meta<'a> {
    file: &'a str,
    title: &'a str,
    subtitle: &'a str,
    provenance: String,
    footnote: String,
}

/// One chart, one or two panels side by side.
fn render(
    out: &Path,
    meta: &Meta<'_>,
    impls: &[&Impl],
    versions: &std::collections::BTreeMap<String, String>,
    panels: &[Panel<'_>],
) -> Result<()> {
    let path = out.join(meta.file);
    let path = path.as_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let root = SVGBackend::new(path, (W, H)).into_drawing_area();
    root.fill(&theme::BACKGROUND)?;

    let (header, rest) = root.split_vertically(HEADER_H);
    theme::draw_header(&header, meta.title, meta.subtitle, &meta.provenance)?;

    let legend_h = theme::legend_height(impls.len(), W as i32).max(LEGEND_MIN_H);
    let (body, tail) = rest.split_vertically(H - HEADER_H - legend_h - FOOTNOTE_H);
    let (legend, note) = tail.split_vertically(legend_h);

    let areas = if panels.len() > 1 {
        body.split_evenly((1, panels.len()))
    } else {
        vec![body]
    };
    for (area, panel) in areas.iter().zip(panels) {
        let (y_max, y_ticks) = theme::nice_axis(theme::series_max(panel.series, impls), 5);
        theme::draw_panel(
            area,
            panel.caption,
            &panel.sizes,
            impls,
            panel.series,
            y_max,
            y_ticks,
            panel.fmt,
        )?;
    }

    theme::draw_legend(&legend, impls, versions, W as i32)?;
    theme::draw_footnote(&note, &meta.footnote, W as i32)?;
    root.present()?;
    println!("wrote {}", path.display());
    Ok(())
}

struct Panel<'a> {
    caption: &'a str,
    sizes: Vec<u64>,
    series: &'a model::Series,
    fmt: &'a dyn Fn(f64) -> String,
}

/// A throughput-family chart: message rate beside byte rate. Both matter and
/// they disagree: an engine that wins on msgs/s at 64 B can lose on GB/s at
/// 16 KiB, and one panel alone hides that.
fn rate_chart(
    a: &Archive,
    out: &Path,
    file: &str,
    title: &str,
    subtitle: &str,
    q: &Query<'_>,
    impls: &[Impl],
) -> Result<()> {
    let msgs = a.series(q, model::msgs);
    let gbps = a.series(q, model::gbps);
    if msgs.is_empty() {
        println!("skip {file}: no cells");
        return Ok(());
    }
    let present = theme::present(&[&msgs, &gbps], impls);
    let sizes = sizes_of(&[&msgs, &gbps]);
    render(
        out,
        &Meta {
            file,
            title,
            subtitle,
            provenance: provenance(a),
            footnote: footnote(a.unstable_count(q)),
        },
        &present,
        &a.versions(),
        &[
            Panel {
                caption: "message rate (higher is better)",
                sizes: sizes.clone(),
                series: &msgs,
                fmt: &theme::fmt_msgs,
            },
            Panel {
                caption: "byte rate (higher is better)",
                sizes,
                series: &gbps,
                fmt: &theme::fmt_gbps,
            },
        ],
    )
}

/// A latency chart: median beside tail. p50 says what a typical call costs, p99
/// says what the worst one in a hundred costs, and for a messaging library the
/// second is usually the number that decides an adoption.
fn latency_chart(
    a: &Archive,
    out: &Path,
    file: &str,
    title: &str,
    subtitle: &str,
    q: &Query<'_>,
    impls: &[Impl],
) -> Result<()> {
    let p50 = a.series(q, model::lat_p50_us);
    let p99 = a.series(q, model::lat_p99_us);
    if p50.is_empty() {
        println!("skip {file}: no cells");
        return Ok(());
    }
    let present = theme::present(&[&p50, &p99], impls);
    let sizes = sizes_of(&[&p50, &p99]);
    render(
        out,
        &Meta {
            file,
            title,
            subtitle,
            provenance: provenance(a),
            footnote: footnote(a.unstable_count(q)),
        },
        &present,
        &a.versions(),
        &[
            Panel {
                caption: "p50 round-trip (lower is better)",
                sizes: sizes.clone(),
                series: &p50,
                fmt: &theme::fmt_us,
            },
            Panel {
                caption: "p99 round-trip (lower is better)",
                sizes,
                series: &p99,
                fmt: &theme::fmt_us,
            },
        ],
    )
}

pub fn render_all(a: &Archive, out: &Path, pubsub_peers: u32, fan_peers: u32) -> Result<()> {
    // ── headline: every implementation, tcp ──
    latency_chart(
        a,
        out,
        "headline_reqrep_tcp.svg",
        "REQ/REP latency over TCP",
        "Round-trip time against payload size. One request in flight at a time.",
        &Query {
            kind: "latency",
            transport: "tcp_netns",
            peers: None,
        },
        IMPLS,
    )?;
    rate_chart(
        a,
        out,
        "headline_pushpull_tcp.svg",
        "PUSH/PULL throughput over TCP",
        "One producer to one consumer, streaming as fast as the pair allows.",
        &Query {
            kind: "throughput",
            transport: "tcp_netns",
            peers: None,
        },
        IMPLS,
    )?;
    rate_chart(
        a,
        out,
        "headline_pubsub_tcp.svg",
        &format!("PUB/SUB throughput over TCP, {pubsub_peers} subscribers"),
        "Aggregate rate out of one publisher broadcasting to every subscriber.",
        &Query {
            kind: "pubsub",
            transport: "tcp_netns",
            peers: Some(pubsub_peers),
        },
        IMPLS,
    )?;

    // ── extended: monocoque against the reference ──
    rate_chart(
        a,
        out,
        "extended_pushpull_ipc.svg",
        "PUSH/PULL throughput over IPC",
        "The same pattern over unix domain sockets.",
        &Query {
            kind: "throughput",
            transport: "ipc",
            peers: None,
        },
        IMPLS,
    )?;
    latency_chart(
        a,
        out,
        "extended_reqrep_ipc.svg",
        "REQ/REP latency over IPC",
        "The same pattern over unix domain sockets.",
        &Query {
            kind: "latency",
            transport: "ipc",
            peers: None,
        },
        IMPLS,
    )?;
    rate_chart(
        a,
        out,
        "extended_fanout_tcp.svg",
        &format!("PUSH fan-out over TCP, {fan_peers} workers"),
        "One producer sharing work across many consumers.",
        &Query {
            kind: "fanout",
            transport: "tcp_netns",
            peers: Some(fan_peers),
        },
        IMPLS,
    )?;
    rate_chart(
        a,
        out,
        "extended_fanin_tcp.svg",
        &format!("PULL fan-in over TCP, {fan_peers} workers"),
        "Many producers converging on one consumer.",
        &Query {
            kind: "fanin",
            transport: "tcp_netns",
            peers: Some(fan_peers),
        },
        IMPLS,
    )?;
    Ok(())
}
