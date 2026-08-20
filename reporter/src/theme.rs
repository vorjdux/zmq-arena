//! Palette, formatting, and the shared panel drawing.
//!
//! Colours match the interactive dashboard so a library is the same colour in
//! the SVGs and in `docs/index.html`, and they match the OMQ comparison charts
//! where the two projects plot the same engine.

use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};

use crate::model::Series;

// ── palette ────────────────────────────────────────────────────
// Dark only, and deliberately so: an SVG cannot follow the reader's colour
// scheme the way the dashboard's CSS can, so it commits to one look rather than
// being illegible in half of them.
pub const BACKGROUND: RGBColor = RGBColor(15, 18, 22); // dashboard --bg
pub const GRID: RGBColor = RGBColor(42, 49, 58); // dashboard --line
pub const AXIS: RGBColor = RGBColor(154, 164, 178); // dashboard --muted
pub const TEXT: RGBColor = RGBColor(230, 233, 238); // dashboard --fg
pub const MUTED: RGBColor = RGBColor(154, 164, 178);

/// One plotted library. `key` is the record's `variant`.
pub struct Impl {
    pub key: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub color: RGBColor,
}

// Engine colours. libzmq takes yellow because it is the ratio baseline and the
// eye should find it first; the rest follow the hues the same engines carry in
// the OMQ comparison charts, so a reader moving between the two projects is not
// re-learning the legend. No engine is styled to stand out from the others.
pub const C_LIBZMQ: RGBColor = RGBColor(250, 204, 21);
pub const C_RUST_ZMQ: RGBColor = RGBColor(245, 158, 11);
// One hue family per engine, one shade per runtime, so a reader can tell at a
// glance which lines are the same engine and which are different engines. Every
// engine gets the same treatment: an evenly spaced light/mid/dark ramp within
// its family, and no engine is given a brighter or more prominent colour.
//
// The ramps are spread wide on purpose. Adjacent shades of one hue are the
// hardest pairs to separate on a dark background, and those are exactly the
// pairs a reader most wants to compare, since they are the same protocol code on
// two different runtimes.
pub const C_ZMQRS: RGBColor = RGBColor(147, 197, 253);
pub const C_ZMQRS_ASYNC_STD: RGBColor = RGBColor(59, 130, 246);
pub const C_ZMQRS_DISPATCHER: RGBColor = RGBColor(30, 64, 175);
pub const C_OMQ: RGBColor = RGBColor(252, 165, 165);
pub const C_OMQ_MT: RGBColor = RGBColor(239, 68, 68);
pub const C_OMQ_BLOCKING: RGBColor = RGBColor(153, 27, 27);
pub const C_MONOCOQUE: RGBColor = RGBColor(94, 234, 212);
pub const C_MONOCOQUE_TOKIO: RGBColor = RGBColor(20, 184, 166);
pub const C_MONOCOQUE_SMOL: RGBColor = RGBColor(15, 100, 95);

/// Every variant the arena knows how to plot, in legend order.
///
/// One list for every chart. There is deliberately no per-chart subset: a chart
/// that hardcoded which libraries may appear would decide the comparison before
/// the data did. Which entries are actually drawn is decided by [`present`],
/// purely from whether the run produced cells for them.
pub const IMPLS: &[Impl] = &[
    Impl {
        key: "libzmq",
        label: "libzmq",
        note: "C++, epoll",
        color: C_LIBZMQ,
    },
    Impl {
        key: "rust_zmq",
        label: "rust-zmq",
        note: "FFI to libzmq",
        color: C_RUST_ZMQ,
    },
    Impl {
        key: "zeromq_rs",
        label: "zmq.rs",
        note: "tokio",
        color: C_ZMQRS,
    },
    Impl {
        key: "zeromq_rs_async_std",
        label: "zmq.rs",
        note: "async-std",
        color: C_ZMQRS_ASYNC_STD,
    },
    Impl {
        key: "zeromq_rs_async_dispatcher",
        label: "zmq.rs",
        note: "async-dispatcher",
        color: C_ZMQRS_DISPATCHER,
    },
    Impl {
        key: "omq_tokio",
        label: "omq",
        note: "tokio CT",
        color: C_OMQ,
    },
    Impl {
        key: "omq_tokio_mt",
        label: "omq",
        note: "tokio MT",
        color: C_OMQ_MT,
    },
    Impl {
        key: "omq_blocking",
        label: "omq",
        note: "blocking",
        color: C_OMQ_BLOCKING,
    },
    Impl {
        key: "monocoque",
        label: "monocoque",
        note: "compio io_uring",
        color: C_MONOCOQUE,
    },
    Impl {
        key: "monocoque_tokio",
        label: "monocoque",
        note: "tokio",
        color: C_MONOCOQUE_TOKIO,
    },
    Impl {
        key: "monocoque_smol",
        label: "monocoque",
        note: "smol",
        color: C_MONOCOQUE_SMOL,
    },
];

// ── formatting ─────────────────────────────────────────────────

pub fn fmt_size(b: u64) -> String {
    if b >= 1024 {
        format!("{} KiB", b / 1024)
    } else {
        format!("{b} B")
    }
}

pub fn fmt_msgs(v: f64) -> String {
    if v >= 1e6 {
        let n = v / 1e6;
        if (n - n.round()).abs() < 0.05 {
            format!("{n:.0}M/s")
        } else {
            format!("{n:.1}M/s")
        }
    } else if v >= 1e3 {
        format!("{:.0}K/s", v / 1e3)
    } else {
        format!("{v:.0}/s")
    }
}

pub fn fmt_gbps(v: f64) -> String {
    if v >= 1.0 {
        format!("{v:.1} GB/s")
    } else if v > 0.0 {
        format!("{:.0} MB/s", v * 1000.0)
    } else {
        String::new()
    }
}

pub fn fmt_us(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0} us")
    } else if v > 0.0 {
        format!("{v:.1} us")
    } else {
        String::new()
    }
}

/// Round an axis maximum up to a readable step, returning the max and the tick
/// count. Without this plotters picks the data maximum and the labels land on
/// values like "3.7M/s".
pub fn nice_axis(max_val: f64, target_lines: usize) -> (f64, usize) {
    if max_val <= 0.0 {
        return (1.0, 2);
    }
    let raw = max_val / target_lines.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    } * mag;
    let ticks = (max_val / step).ceil().max(1.0);
    (step * ticks, ticks as usize)
}

/// Largest value across the variants actually being plotted.
pub fn series_max(series: &Series, impls: &[&Impl]) -> f64 {
    series
        .values()
        .flat_map(|per_variant| {
            impls
                .iter()
                .filter_map(move |i| per_variant.get(i.key).copied())
        })
        .fold(0.0f64, f64::max)
}

/// The variants that have at least one point, in legend order. Drawing from the
/// full list would put dead entries in the legend for a run where an engine did
/// not report.
pub fn present<'a>(series: &[&Series], impls: &'a [Impl]) -> Vec<&'a Impl> {
    impls
        .iter()
        .filter(|i| {
            series
                .iter()
                .any(|s| s.values().any(|per| per.contains_key(i.key)))
        })
        .collect()
}

pub fn font(size: i32, color: RGBColor) -> TextStyle<'static> {
    ("sans-serif", size).into_font().color(&color)
}

/// Draw one payload-sweep panel: x is the payload size (categorical, so the
/// sweep points are evenly spaced regardless of their ratios), y is the metric.
#[allow(clippy::too_many_arguments)]
pub fn draw_panel(
    area: &DrawingArea<SVGBackend<'_>, plotters::coord::Shift>,
    caption: &str,
    sizes: &[u64],
    impls: &[&Impl],
    series: &Series,
    y_max: f64,
    y_ticks: usize,
    y_fmt: &dyn Fn(f64) -> String,
) -> anyhow::Result<()> {
    let mut chart = ChartBuilder::on(area)
        .caption(caption, font(12, TEXT))
        .set_label_area_size(LabelAreaPosition::Bottom, 30)
        .set_label_area_size(LabelAreaPosition::Left, 74)
        .margin_top(34)
        .margin_left(10)
        .margin_right(22)
        .build_cartesian_2d(0.0..(sizes.len().max(2) - 1) as f64, 0.0..y_max)?;

    chart
        .configure_mesh()
        .x_labels(sizes.len())
        .x_label_formatter(&|v| {
            sizes
                .get(v.round() as usize)
                .map_or(String::new(), |&s| fmt_size(s))
        })
        .y_labels(y_ticks + 1)
        .y_label_formatter(&|v| y_fmt(*v))
        .x_label_style(font(10, TEXT))
        .y_label_style(font(10, TEXT))
        .light_line_style(TRANSPARENT)
        .bold_line_style(GRID)
        .axis_style(AXIS)
        .draw()?;

    // Reverse order so the first legend entry is drawn last and sits on top.
    for imp in impls.iter().rev() {
        let pts: Vec<(f64, f64)> = sizes
            .iter()
            .enumerate()
            .filter_map(|(i, s)| series.get(s)?.get(imp.key).map(|&v| (i as f64, v)))
            .collect();
        if pts.is_empty() {
            continue;
        }
        chart.draw_series(LineSeries::new(
            pts.iter().copied(),
            imp.color.stroke_width(2),
        ))?;
        chart.draw_series(
            pts.iter()
                .map(|&(x, y)| Circle::new((x, y), 2, imp.color.filled())),
        )?;
    }
    Ok(())
}

/// Title block at the top of a chart: what it measures, then the provenance
/// line. The provenance is not decoration. A reader who cannot see which host
/// and run produced a number cannot tell a verdict from a smoke test.
pub fn draw_header(
    area: &DrawingArea<SVGBackend<'_>, plotters::coord::Shift>,
    title: &str,
    subtitle: &str,
    provenance: &str,
) -> anyhow::Result<()> {
    area.draw_text(title, &font(17, TEXT), (18, 14))?;
    area.draw_text(subtitle, &font(11, MUTED), (18, 36))?;
    area.draw_text(provenance, &font(10, MUTED), (18, 52))?;
    Ok(())
}

/// Legend along the bottom: a colour swatch, the library, its runtime note and
/// the measured version.
///
/// Wraps into as many rows as it needs. With every runtime an engine ships
/// getting its own series there can be a dozen entries, and a single row would
/// either overflow the page or truncate the labels that distinguish one runtime
/// of an engine from another, which are exactly the labels a reader needs.
pub fn draw_legend(
    area: &DrawingArea<SVGBackend<'_>, plotters::coord::Shift>,
    impls: &[&Impl],
    versions: &std::collections::BTreeMap<String, String>,
    width: i32,
) -> anyhow::Result<()> {
    const MIN_COL: i32 = 160;
    const ROW_H: i32 = 30;
    let usable = width - 2 * LEGEND_PAD;
    let cols = (usable / MIN_COL).clamp(1, impls.len().max(1) as i32);
    let per = usable / cols;
    for (i, imp) in impls.iter().enumerate() {
        let i = i as i32;
        let x = LEGEND_PAD + per * (i % cols);
        let y = 8 + ROW_H * (i / cols);
        area.draw(&Rectangle::new(
            [(x, y), (x + 10, y + 10)],
            imp.color.filled(),
        ))?;
        let ver = versions
            .get(imp.key)
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        area.draw_text(
            &format!("{}{ver}", imp.label),
            &font(11, TEXT),
            (x + 16, y - 1),
        )?;
        if !imp.note.is_empty() {
            area.draw_text(imp.note, &font(9, MUTED), (x + 16, y + 13))?;
        }
    }
    Ok(())
}

/// Left and right margin of the legend block, matching the header's.
const LEGEND_PAD: i32 = 18;

/// How tall the legend needs to be for this many entries at this width, so the
/// caller can reserve the right amount before splitting the drawing area.
pub fn legend_height(count: usize, width: i32) -> u32 {
    const MIN_COL: i32 = 160;
    const ROW_H: i32 = 30;
    let cols = ((width - 2 * LEGEND_PAD) / MIN_COL).clamp(1, count.max(1) as i32);
    let rows = (count as i32 + cols - 1) / cols;
    (8 + ROW_H * rows) as u32
}

/// Centred footnote, used for the "cells did not converge" caveat.
pub fn draw_footnote(
    area: &DrawingArea<SVGBackend<'_>, plotters::coord::Shift>,
    text: &str,
    width: i32,
) -> anyhow::Result<()> {
    let style = font(9, MUTED).pos(Pos::new(HPos::Center, VPos::Top));
    area.draw_text(text, &style, (width / 2, 4))?;
    Ok(())
}
