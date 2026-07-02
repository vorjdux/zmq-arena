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
import math
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
    "monocoque_tokio": {"engine": "monocoque", "io": "epoll",    "threading": "single"},
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

    # Syscall honesty. Two problems the raw per-cell counts hide:
    #  1. io_uring_enter is a batched syscall (one enter reaps many completions),
    #     so a raw count is not comparable to an epoll engine's per-readiness
    #     epoll_wait. We normalise to per-1k-messages so the amortisation is
    #     visible and the columns are commensurable.
    #  2. On an unprivileged host the perf tracepoints do not register and every
    #     counter reads 0. That is "not measured", not "zero syscalls", so we mark
    #     the whole block uncaptured rather than letting a 0 read as a real value.
    syscall_names = ("epoll_wait", "epoll_ctl", "sendmsg", "recvmsg", "io_uring_enter")
    syscalls_captured = any(sysc.get(k, 0) for k in syscall_names)
    # Messages that flowed during the probe window (it spans warmup + measured).
    # Count-based kinds carry the counts; duration kinds derive it from the rate.
    basis = entry.get("messages", 0) + entry.get("warmup_messages", 0)
    if basis == 0 and throughput and entry.get("duration_secs"):
        basis = int(throughput["msgs_per_s"] * entry["duration_secs"])
    if syscalls_captured and basis > 0:
        per_k = {k: round(sysc.get(k, 0) / (basis / 1000.0), 3) for k in syscall_names}
    else:
        per_k = None

    # Replication spread of the primary metric. Present on records the replicated
    # orchestrator wrote; absent on legacy single-shot records, which we surface as
    # a single unstable-of-unknown-spread sample so the dashboard can still tell
    # "one draw" apart from "converged estimate".
    st = cell.get("stability")
    if st:
        stability = {
            "n": st.get("n", 0),
            "replicates": st.get("replicates", 0),
            "outliers_dropped": st.get("outliers_dropped", 0),
            "median": st.get("median", 0.0),
            "iqr": st.get("iqr", 0.0),
            "rel_iqr": st.get("rel_iqr", 0.0),
            "cv": st.get("cv", 0.0),
            "min": st.get("min", 0.0),
            "max": st.get("max", 0.0),
            "stable": bool(st.get("stable", False)),
            # Set by flag_inversions() once every record is built (it needs the
            # whole payload sweep for a variant). Defaults to "not inverted".
            "inverted": False,
        }
    else:
        stability = {
            "n": 1, "replicates": 1, "outliers_dropped": 0,
            "median": 0.0, "iqr": 0.0, "rel_iqr": 0.0, "cv": 0.0,
            "min": 0.0, "max": 0.0, "stable": False, "inverted": False,
        }

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
        "syscalls_captured": syscalls_captured,
        "syscalls_per_kmsg": per_k,
        # Messages that flowed in the measured window, so the efficiency board can
        # form CPU-per-message (0 when unknown, e.g. a duration cell with no rate).
        "messages_basis": basis,
        "sched": {
            "voluntary": sched.get("voluntary_ctxt_switches", 0),
            "involuntary": sched.get("involuntary_ctxt_switches", 0),
        },
        "peak_memory_bytes": cell.get("peak_memory_bytes", 0),
        "stability": stability,
    }


def load_cells(scratch: Path) -> list:
    files = sorted(scratch.glob("*.json"))
    if not files:
        print(f"ERROR: no *.json cell records in {scratch}", file=sys.stderr)
        sys.exit(1)
    return [json.loads(f.read_text()) for f in files]


# The throughput-family kinds, all measured in msgs/s.
THROUGHPUT_KINDS = {"throughput", "pubsub", "fanout", "fanin"}


def flag_inversions(records: list, margin: float = 0.15) -> None:
    """Correctness check the stability flag cannot provide.

    Throughput in msgs/s must fall as the payload grows: a bigger message can
    never carry at a higher message rate on the same path. So within one
    (variant, kind, transport, peers) sweep, a cell whose rate is beaten by a
    LARGER payload is physically suspect, usually a measurement or socket-config
    artifact (the monocoque TCP 64 B Nagle case is the canonical example). Such a
    cell is flagged inverted.

    This is orthogonal to the stability flag: a cell can be perfectly reproducible
    (stable) and still be inverted, because reproducibility is not correctness. A
    `margin` guards against flagging noise-level swaps between adjacent points; a
    larger payload must beat this cell by more than the margin to count.
    """
    groups = {}
    for r in records:
        if r["kind"] not in THROUGHPUT_KINDS or not r.get("throughput"):
            continue
        key = (r["variant"], r["kind"], r["transport"], r.get("peers"))
        groups.setdefault(key, []).append(r)
    for rs in groups.values():
        rs.sort(key=lambda r: r["payload_bytes"])
        rates = [r["throughput"]["msgs_per_s"] for r in rs]
        for i, r in enumerate(rs):
            rate = rates[i]
            larger_max = max(rates[i + 1:], default=0.0)
            inverted = rate > 0 and larger_max > rate * (1 + margin)
            r.setdefault("stability", {})["inverted"] = bool(inverted)


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
    lines.append(f"| # | variant | {label} ({unit}) | spread | n | conf |")
    lines.append("|---|---------|------|--------|---|------|")
    any_unstable = False
    any_inverted = False
    for i, r in enumerate(rs, 1):
        v = metric_path(r)
        stab = r.get("stability") or {}
        rel = stab.get("rel_iqr", 0.0)
        n = stab.get("n", 1)
        stable = stab.get("stable", False)
        inverted = stab.get("inverted", False)
        # Inverted outranks low: a physically-suspect value is worse news than a
        # merely noisy one, so it wins the conf label.
        if inverted:
            conf = "INVERTED"
            any_inverted = True
        elif stable:
            conf = "ok"
        else:
            conf = "low"
            any_unstable = True
        lines.append(f"| {i} | {r['variant']} | {v:.2f} | {rel * 100:.1f}% | {n} | {conf} |")
    lines.append("")
    if any_unstable:
        lines.append("> conf=low means the cell's replicates did not converge "
                     "(relative IQR above target); treat its rank as indicative, "
                     "not decisive.")
        lines.append("")
    if any_inverted:
        lines.append("> conf=INVERTED means the msgs/s here is beaten by a larger "
                     "payload in the same sweep, which is physically impossible on "
                     "one path; the number is a measurement or socket-config "
                     "artifact, not a real result. Reproducible does not mean "
                     "correct.")
        lines.append("")
    return "\n".join(lines)


# The reference implementation every score is normalized against. It is present
# in every cell of the grid (the C++ libzmq is the ubiquitous baseline), so every
# variant can be compared to it on the cells they share.
BASELINE = "libzmq"


def _rel_spread(r):
    """Relative replicate spread (IQR / median) of the cell's primary metric, the
    dimensionless noise band used for significance gating. 0 when unknown."""
    return (r.get("stability") or {}).get("rel_iqr", 0.0) or 0.0


def geomean_board(records, cell_ok, value, gated=True, baseline=BASELINE):
    """Rank variants by the geometric mean of their per-cell ratio to the baseline.

    This is the SPEC/PARSEC-style aggregation: scale-invariant, magnitude-aware
    (a 3x win counts as 3x, not just "1st place"), and comparable across kinds
    because every ratio is dimensionless. `value(r)` returns a higher-is-better
    score (reciprocal already applied for lower-is-better metrics), or None to
    skip. `cell_ok(r)` selects the cells for this board (one dimension).

    Fairness rules baked in:
      - Cells flagged `inverted` (a physically impossible result, e.g. beaten by a
        larger payload) are dropped: they are known-wrong, not slow.
      - Only cells shared with the baseline count, so a variant that skips the hard
        benchmarks cannot flatter its own average (coverage is reported, not hidden).
      - Significance gating: a variant only earns credit for beating the baseline
        in a cell when the gap exceeds the two cells' combined replicate spread.
        Otherwise the ratio is pinned to 1.0 (a tie). Noisy, non-converged
        (unstable) cells have a wide band, so they rarely move the score, which is
        how a reproducible-but-uncertain number is kept from deciding the ranking.

    Returns rows of (variant, geomean_x, shared_cells, gated_ties).
    """
    groups = {}
    for r in records:
        if not cell_ok(r) or (r.get("stability") or {}).get("inverted"):
            continue
        s = value(r)
        if s is None or s <= 0:
            continue
        key = (r["kind"], r["transport"], r["payload_bytes"], r.get("peers"))
        groups.setdefault(key, {})[r["variant"]] = (s, _rel_spread(r))

    acc = {}  # variant -> [log-sum, n, gated]
    for d in groups.values():
        if baseline not in d:
            continue
        base_s, base_sp = d[baseline]
        for v, (s, sp) in d.items():
            ratio = s / base_s
            a = acc.setdefault(v, [0.0, 0, 0])
            if gated and abs(ratio - 1.0) <= (sp + base_sp):
                ratio = 1.0        # gap within the combined noise: call it a tie
                a[2] += 1
            a[0] += math.log(ratio)
            a[1] += 1
    rows = [(v, math.exp(ls / n), n, g) for v, (ls, n, g) in acc.items() if n]
    rows.sort(key=lambda x: -x[1])
    return rows


def board_table(title, note, rows, total_cells):
    """Render one geomean board as a markdown section. `total_cells` is the max
    cells any variant could share with the baseline in this dimension, so a short
    coverage count is visible rather than silently flattering a selective variant."""
    if not rows:
        return None
    lines = [f"## {title}", "", note, "",
             "| # | variant | vs libzmq | cells | ties |",
             "|---|---------|-----------|-------|------|"]
    for i, (v, g, n, gated) in enumerate(rows, 1):
        cov = f"{n}/{total_cells}" + (" (partial)" if n < total_cells else "")
        lines.append(f"| {i} | {v} | {g:.2f}x | {cov} | {gated} |")
    lines.append("")
    return "\n".join(lines)


def _distinct_cells(records, cell_ok):
    """How many distinct (kind, transport, payload, peers) cells in this dimension
    also contain the baseline: the denominator for the coverage column."""
    keys = set()
    for r in records:
        if cell_ok(r) and r["variant"] == BASELINE and not (r.get("stability") or {}).get("inverted"):
            keys.add((r["kind"], r["transport"], r["payload_bytes"], r.get("peers")))
    return len(keys)


def write_ranking(repo: Path, date: str, records: list):
    is_lat = lambda r: bool(r.get("latency_ns"))
    is_tput = lambda r: r["kind"] in THROUGHPUT_KINDS and bool(r.get("throughput"))
    has_eff = lambda r: r.get("cpu_seconds", 0) > 0 and r.get("messages_basis", 0) > 0

    # Higher-is-better score per dimension (reciprocal for lower-is-better metrics).
    lat_score = lambda r: (1.0 / r["latency_ns"]["p99"]) if r["latency_ns"]["p99"] else None
    tput_score = lambda r: r["throughput"]["msgs_per_s"]
    cpu_eff = lambda r: r["messages_basis"] / r["cpu_seconds"]      # msgs per CPU-second
    mem_eff = lambda r: (1.0 / r["peak_memory_bytes"]) if r.get("peak_memory_bytes") else None

    parts = [
        "# Ranking\n",
        f"From the run on {date}. This file is rewritten on every run; for the full "
        f"history and interactive charts, open the dashboard under `docs/`.\n",
        "> **Read this first.** These boards are only as good as the host they ran "
        "on. When the producer and consumer share a single core (the dev-host and "
        "default CI matrix), the numbers rank mechanics and core-contention, not a "
        "library's absolute performance; a multi-threaded engine is penalised for "
        "contending with itself. Treat a pinned, multi-core `run-root` grid as the "
        "only source of a real verdict.\n",
        "Each board is the **geometric mean of every variant's ratio to the "
        f"`{BASELINE}` baseline**, over the cells they share. This is magnitude-aware "
        "(a 3x win counts as 3x, unlike averaging rank positions) and dimensionless "
        "(so payloads and transports combine cleanly). Cells flagged as inverted are "
        "dropped as known-wrong; a win smaller than the two cells' combined replicate "
        "spread is counted as a tie (the `ties` column), so noisy cells do not decide "
        "the order. Higher is better on every board. `cells` shows coverage against "
        "the baseline; a partial count means the variant did not run every benchmark "
        "and its score is not directly comparable to a full-coverage one.\n",
    ]

    boards = [
        ("Latency (p99, lower raw is better)",
         "Reciprocal p99 latency, so higher on this board is faster.",
         is_lat, lat_score, True),
        ("Throughput (msgs/s)",
         "Message rate across throughput, pub/sub, fan-out and fan-in.",
         is_tput, tput_score, True),
        ("CPU efficiency (messages per CPU-second)",
         "Work done per core-second across the whole cell (both processes). "
         "Rewards doing the same traffic for less CPU, which raw throughput hides.",
         has_eff, cpu_eff, False),
        ("Memory (footprint)",
         "Reciprocal peak RSS across the cell's processes, so higher is leaner.",
         lambda r: r.get("peak_memory_bytes", 0) > 0, mem_eff, False),
    ]
    any_board = False
    for title, note, ok, val, gated in boards:
        rows = geomean_board(records, ok, val, gated=gated)
        t = board_table(title, note, rows, _distinct_cells(records, ok))
        if t:
            parts.append(t)
            any_board = True
    if not any_board:
        parts.append("_No comparable cells in this run._\n")

    # Per-(kind, transport) detail tables at the smallest payload, for the granular
    # view. Inverted rows are flagged in the conf column by rank_table.
    p99_us = lambda r: (r["latency_ns"]["p99"] / 1000) if r.get("latency_ns") else None
    msgs = lambda r: (r["throughput"]["msgs_per_s"]) if r.get("throughput") else None
    parts.append("## Per-benchmark detail\n")
    for kind, transport in [("latency", "ipc"), ("latency", "tcp_netns")]:
        t = rank_table(records, kind, transport, p99_us, True, "p99 latency", "µs")
        if t:
            parts.append(t)
    for kind, transport in [("throughput", "ipc"), ("throughput", "tcp_netns")]:
        t = rank_table(records, kind, transport, msgs, False, "throughput", "msg/s")
        if t:
            parts.append(t)
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
    flag_inversions(records)  # payload-monotonicity correctness check across the sweep
    hardware = {"cpu": args.hardware_cpu, "note": args.hardware_note}

    fname = write_archive(args.docs, run_id, date, hardware, records)
    write_ranking(args.repo, date, records)
    print(f"rendered {len(records)} records -> docs/history/{fname}, updated index.json + RANKING.md")


if __name__ == "__main__":
    main()
