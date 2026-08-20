//! zmq-arena-report: render a run archive into static SVG charts.
//!
//! Usage:
//!   zmq-arena-report --run docs/history/2026-07-03-run.json --out docs/charts
//!   zmq-arena-report --latest docs/history --out docs/charts
//!
//! The interactive dashboard under `docs/` reads the same archives and owns
//! history drill-down. These charts are the fixed, linkable picture: what the
//! README shows and what a reader gets without a browser and a web server.

mod charts;
mod model;
mod theme;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "zmq-arena-report",
    about = "Render zmq-arena run archives to SVG charts"
)]
struct Cli {
    /// Run archive to chart.
    #[arg(long, conflicts_with = "latest")]
    run: Option<PathBuf>,
    /// Directory of archives; charts the newest by date.
    #[arg(long)]
    latest: Option<PathBuf>,
    /// Where the SVGs go.
    #[arg(long, default_value = "docs/charts")]
    out: PathBuf,
    /// Subscriber count of the headline pub/sub cell, matching `gen_matrix.py`.
    #[arg(long, default_value_t = 32)]
    pubsub_peers: u32,
    /// Worker count of the fan-out and fan-in cells, matching `gen_matrix.py`.
    #[arg(long, default_value_t = 4)]
    fan_peers: u32,
}

/// Newest archive in a directory, by the run manifest's date ordering. Falls
/// back to filename order when there is no manifest, since the archives are
/// named `<date>-run.json`.
fn newest(dir: &Path) -> Result<PathBuf> {
    let mut runs: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-run.json"))
        })
        .collect();
    runs.sort();
    runs.pop()
        .with_context(|| format!("no *-run.json archives in {}", dir.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = match (&cli.run, &cli.latest) {
        (Some(p), _) => p.clone(),
        (None, Some(d)) => newest(d)?,
        (None, None) => bail!("pass --run <archive.json> or --latest <dir>"),
    };

    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let archive: model::Archive =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    println!(
        "charting {} ({} records) -> {}",
        path.display(),
        archive.records.len(),
        cli.out.display()
    );
    charts::render_all(&archive, &cli.out, cli.pubsub_peers, cli.fan_peers)
}
