#!/usr/bin/env python3
"""Render orchestrator output into the dashboard archive + RANKING.md.

Input:  a scratch directory of per-cell JSON records as emitted by the
        orchestrator (`zmq-arena run --out <scratch>`), one file per cell in the
        `CellRecord` shape.
Output: docs/history/<date>-run.json   (dashboard archive schema)
        docs/history/index.json        (manifest, appended)
        RANKING.md                     (static ledger, overwritten)

This is the CI "render step". It is pure data transformation, no measurement.

Usage:
  python3 scripts/render_results.py --scratch scratch/2026-06-29 --run-id 2026-06-29
"""

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# (target id, variant) -> dashboard variant key. Variant None/"default" keeps the
# target id. Keep in sync with scripts/gen_sample_history.py and docs/index.html.
VARIANT_KEY = {
    ("omq_tokio", "multi_thread"): "omq_tokio_mt",
    ("omq_compio", "single_thread"): "omq_compio_st",
    ("monocoque", "tokio"): "monocoque_tokio",
}
# Category tags per variant key.
REGISTRY = {
    "libzmq":        {"engine": "libzmq",    "io": "epoll",    "threading": "native"},
    "rust_zmq":      {"engine": "libzmq",    "io": "epoll",    "threading": "native"},
    "zmq.rs":        {"engine": "zmq.rs",    "io": "epoll",    "threading": "multi"},
    "zeromq_rs":     {"engine": "zmq.rs",    "io": "epoll",    "threading": "multi"},
    "omq_tokio":     {"engine": "omq",       "io": "epoll",    "threading": "single"},
    "omq_tokio_mt":  {"engine": "omq",       "io": "epoll",    "threading": "multi"},
    "omq_compio":    {"engine": "omq",       "io": "io_uring", "threading": "single"},
    "omq_compio_st": {"engine": "omq",       "io": "io_uring", "threading": "single"},
    "rzmq":          {"engine": "rzmq",      "io": "io_uring", "threading": "multi"},
    "celerity":      {"engine": "celerity",  "io": "epoll",    "threading": "multi"},
    "monocoque":       {"engine": "monocoque", "io": "io_uring", "threading": "single"},
    "monocoque_tokio": {"engine": "monocoque", "io": "epoll",    "threading": "multi"},
}


def variant_key(target_id: str, variant) -> str:
    if variant in (None, "", "default"):
        return target_id
    return VARIANT_KEY.get((target_id, variant), f"{target_id}_{variant}")


def meta(vkey: str) -> dict:
    return REGISTRY.get(vkey, {"engine": vkey, "io": "unknown", "threading": "unknown"})


def to_archive_record(cell: dict) -> dict:
    """Map one orchestrator CellRecord into a dashboard archive record."""
    entry = cell["entry"]
    target = entry["target"]
    vkey = variant_key(target["id"], target.get("variant"))
    m = meta(vkey)
    kind = entry["kind"]

    lat = cell.get("latency") or {}
    latency_ns = None
    throughput = None
    if kind == "latency":
        latency_ns = {
            "min": lat.get("min_ns", 0), "p50": lat.get("p50_ns", 0),
            "p90": lat.get("p90_ns", 0), "p99": lat.get("p99_ns", 0),
            "p999": lat.get("p999_ns", 0), "max": lat.get("max_ns", 0),
        }
    else:
        t = cell.get("throughput") or {}
        throughput = {"msgs_per_s": t.get("msgs_per_s", 0.0), "mbps": t.get("mbps", 0.0)}

    sysc = cell.get("syscalls") or {}
    sched = cell.get("sched") or {}

    # The target is the source of truth: prefer the `meta` block it reported via
    # `describe`, and fall back to the static REGISTRY for engine/io/threading
    # when an older record has no meta. Language falls back to the variant key
    # (only the C++ libzmq_cpp_target is C++; the rest, including the rust-zmq
    # binding to the same core, are Rust).
    tm = cell.get("meta") or {}
    engine = tm.get("engine") or m["engine"]
    io = tm.get("io") or m["io"]
    threading = tm.get("threading") or m["threading"]
    language = tm.get("language") or ("C++" if vkey == "libzmq" else "Rust")
    return {
        "variant": vkey, "engine": engine, "io": io, "threading": threading,
        "language": language,
        "lib_version": tm.get("lib_version", ""),
        "binding_version": tm.get("binding_version"),
        "lib_language": tm.get("lib_language", language),
        "impl": tm.get("impl", ""),
        "ffi_to": tm.get("ffi_to"),
        "concurrency": tm.get("concurrency", ""),
        "kind": kind, "transport": entry["transport"],
        "payload_bytes": entry["payload_bytes"], "peers": entry.get("peers"),
        "latency_ns": latency_ns, "throughput": throughput,
        "cpu_seconds": cell.get("cpu_seconds", 0.0),
        "syscalls": {
            "epoll_wait": sysc.get("epoll_wait", 0), "epoll_ctl": sysc.get("epoll_ctl", 0),
            "sendmsg": sysc.get("sendmsg", 0), "recvmsg": sysc.get("recvmsg", 0),
            "io_uring_enter": sysc.get("io_uring_enter", 0),
        },
        "sched": {
            "voluntary": sched.get("voluntary_ctxt_switches", 0),
            "involuntary": sched.get("involuntary_ctxt_switches", 0),
        },
        "peak_memory_bytes": cell.get("peak_memory_bytes", 0),
    }


def load_cells(scratch: Path) -> list:
    files = sorted(scratch.glob("*.json"))
    if not files:
        print(f"ERROR: no *.json cell records in {scratch}", file=sys.stderr)
        sys.exit(1)
    return [json.loads(f.read_text()) for f in files]


def write_archive(docs: Path, run_id: str, date: str, hardware: dict, records: list) -> str:
    hist = docs / "history"
    hist.mkdir(parents=True, exist_ok=True)
    fname = f"{date}-run.json"
    run = {"run_id": run_id, "date": date, "hardware": hardware, "records": records}
    (hist / fname).write_text(json.dumps(run, separators=(",", ":")))

    index_path = hist / "index.json"
    manifest = {"schema": 2, "sample": False, "note": "Real weekly-grid runs.", "runs": []}
    if index_path.exists():
        try:
            manifest = json.loads(index_path.read_text())
        except json.JSONDecodeError:
            pass
    runs = {r["date"]: r for r in manifest.get("runs", [])}
    runs[date] = {"date": date, "file": fname}
    manifest["runs"] = sorted(runs.values(), key=lambda r: r["date"])
    manifest["sample"] = False
    index_path.write_text(json.dumps(manifest, indent=2))
    return fname


def rank_table(records, kind, transport, metric_path, lower_better, label, unit):
    """Build a markdown ranking table for one (kind, transport) at the smallest
    payload, sorted by metric. metric_path is a callable record->value or None."""
    rs = [r for r in records if r["kind"] == kind and r["transport"] == transport]
    if not rs:
        return None
    payload = min(r["payload_bytes"] for r in rs)
    rs = [r for r in rs if r["payload_bytes"] == payload and metric_path(r) is not None]
    if not rs:
        return None
    rs.sort(key=lambda r: metric_path(r), reverse=not lower_better)
    direction = "lower is better" if lower_better else "higher is better"
    lines = [f"### {label}: {kind}, {transport}, {payload} B ({direction})", ""]
    lines.append(f"| # | variant | {label} ({unit}) |")
    lines.append("|---|---------|------|")
    for i, r in enumerate(rs, 1):
        v = metric_path(r)
        lines.append(f"| {i} | {r['variant']} | {v:.2f} |")
    lines.append("")
    return "\n".join(lines)


def global_ranking(records):
    """One leaderboard across every benchmark. Within each (kind, transport,
    payload, peers) group, rank the variants by that group's primary metric
    (p99 for latency, msgs/s for the throughput family), then average each
    variant's rank position across the groups it appeared in. Averaging rank
    positions, not raw values, keeps the units commensurable across kinds; lower
    mean rank is better. Returns rows of (variant, mean_rank, benchmarks)."""
    def primary(r):
        if r.get("latency_ns"):
            return r["latency_ns"]["p99"], True   # lower is better
        if r.get("throughput"):
            return r["throughput"]["msgs_per_s"], False
        return None, False

    groups = {}
    for r in records:
        val, _ = primary(r)
        if val is None:
            continue
        key = (r["kind"], r["transport"], r["payload_bytes"], r.get("peers"))
        groups.setdefault(key, []).append(r)

    ranks = {}  # variant -> list of positions
    for rs in groups.values():
        lower = primary(rs[0])[1]
        rs = sorted(rs, key=lambda r: primary(r)[0], reverse=not lower)
        for pos, r in enumerate(rs, 1):
            ranks.setdefault(r["variant"], []).append(pos)

    rows = [(v, sum(ps) / len(ps), len(ps)) for v, ps in ranks.items()]
    rows.sort(key=lambda x: x[1])
    return rows


def write_ranking(repo: Path, date: str, records: list):
    p99_us = lambda r: (r["latency_ns"]["p99"] / 1000) if r.get("latency_ns") else None
    msgs = lambda r: (r["throughput"]["msgs_per_s"]) if r.get("throughput") else None

    parts = [f"# Ranking\n",
             f"From the run on {date}. This file is rewritten on every run; for "
             f"the full history and interactive charts, open the dashboard under "
             f"`docs/`.\n"]

    grows = global_ranking(records)
    if grows:
        parts.append("## Global ranking")
        parts.append("")
        parts.append("Mean rank position across every benchmark in the run (lower "
                     "is better). Each benchmark ranks its variants, then the "
                     "positions are averaged, so latency and throughput cells "
                     "count equally.")
        parts.append("")
        parts.append("| # | variant | mean rank | benchmarks |")
        parts.append("|---|---------|-----------|------------|")
        for i, (v, mr, n) in enumerate(grows, 1):
            parts.append(f"| {i} | {v} | {mr:.2f} | {n} |")
        parts.append("")

    any_table = False
    for kind, transport in [("latency", "ipc"), ("latency", "tcp_netns")]:
        t = rank_table(records, kind, transport, p99_us, True, "p99 latency", "µs")
        if t:
            parts.append(t); any_table = True
    for kind, transport in [("throughput", "ipc"), ("throughput", "tcp_netns")]:
        t = rank_table(records, kind, transport, msgs, False, "throughput", "msg/s")
        if t:
            parts.append(t); any_table = True
    if not any_table:
        parts.append("_No comparable cells in this run._\n")
    (repo / "RANKING.md").write_text("\n".join(parts))


def main():
    ap = argparse.ArgumentParser(description="Render orchestrator output for the dashboard")
    ap.add_argument("--scratch", required=True, type=Path)
    ap.add_argument("--run-id", default=None)
    ap.add_argument("--date", default=None, help="archive date (default: run-id or UTC today)")
    ap.add_argument("--docs", default=REPO / "docs", type=Path)
    ap.add_argument("--repo", default=REPO, type=Path)
    ap.add_argument("--hardware-cpu", default="unknown host")
    ap.add_argument("--hardware-note", default="")
    args = ap.parse_args()

    run_id = args.run_id or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    date = args.date or run_id
    cells = load_cells(args.scratch)
    records = [to_archive_record(c) for c in cells]
    hardware = {"cpu": args.hardware_cpu, "note": args.hardware_note}

    fname = write_archive(args.docs, run_id, date, hardware, records)
    write_ranking(args.repo, date, records)
    print(f"rendered {len(records)} records -> docs/history/{fname}, updated index.json + RANKING.md")


if __name__ == "__main__":
    main()
