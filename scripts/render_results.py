#!/usr/bin/env python3
"""Render orchestrator output into the dashboard archive.

Input:  a scratch directory of per-cell JSON records as emitted by the
        orchestrator (`zmq-arena run --out <scratch>`), one file per cell in the
        `CellRecord` shape.
Output: docs/history/<date>-run.json   (dashboard archive schema)
        docs/history/index.json        (manifest, appended)

This is the CI "render step". It is pure data transformation, no measurement.

The archive is the single canonical form of a run. Nothing here ranks or
summarises: the dashboard does that from this data, so there is one dataset and
one thing that interprets it. A second renderer writing a markdown ledger meant
two implementations of the same statistics, in two languages, drifting apart.

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
    ("omq_tokio", "blocking"): "omq_blocking",
    ("monocoque", "tokio"): "monocoque_tokio",
}
# Category tags per variant key, read from variants.json rather than repeated
# here. That file is the single source of truth the dashboard pages and the SVG
# pages also read; keeping a second copy in this script is how a variant ends
# up classified one way in a record and another way in the UI.
def _load_registry() -> dict:
    data = json.loads((REPO / "variants.json").read_text())
    return {
        v["key"]: {"engine": v["engine"], "io": v["io"], "threading": v["threading"]}
        for v in data["variants"]
    }


REGISTRY = _load_registry()


def variant_key(target_id: str, variant) -> str:
    if variant in (None, "", "default"):
        return target_id
    return VARIANT_KEY.get((target_id, variant), f"{target_id}_{variant}")


def meta(vkey: str) -> dict:
    return REGISTRY.get(vkey, {"engine": vkey, "io": "unknown", "threading": "unknown"})


def build_id(vkey: str, engine: str, lib_version: str, binding_version) -> str:
    """Stable identity for one *build* of a variant: what ran, at what version.

    A variant key like `tmq` says which series a cell belongs to; it does not say
    which version produced the number. The archive needs both, because an archive
    is history: when monocoque goes 0.4.0 -> 0.5.0, last month's run must keep
    reporting 0.4.0 forever. So the id carries every version in the measured
    stack, and changes the moment any of them does:

        monocoque@0.4.0                 a native engine, its own version
        tmq@0.5.0+libzmq-4.3.4          a binding, its version AND the engine's

    Including the engine version for bindings is the part that is easy to get
    wrong. `tmq@0.5.0` alone would stay identical across a libzmq 4.3.4 -> 4.3.5
    upgrade, silently merging two different measured stacks under one id.
    """
    own = binding_version or lib_version or "unknown"
    ident = f"{vkey}@{own}"
    if binding_version and lib_version:
        ident += f"+{engine}-{lib_version}"
    return ident


def build_of(cell: dict) -> tuple:
    """The (id, classification) pair for the build that produced this cell.

    Returned separately from the measurement so the archive can list each build
    once. Everything here is a property of the binary that ran, never of the
    individual cell, which is exactly why repeating it per record was waste.
    """
    target = cell["entry"]["target"]
    vkey = variant_key(target["id"], target.get("variant"))
    m = meta(vkey)
    tm = cell.get("meta") or {}
    engine = tm.get("engine") or m["engine"]
    language = tm.get("language") or ("C++" if vkey == "libzmq" else "Rust")
    lib_version = tm.get("lib_version", "")
    binding_version = tm.get("binding_version")
    ident = build_id(vkey, engine, lib_version, binding_version)
    return ident, {
        "variant": vkey,
        "engine": engine,
        "io": tm.get("io") or m["io"],
        "threading": tm.get("threading") or m["threading"],
        "language": language,
        "lib_version": lib_version,
        "binding_version": binding_version,
        "lib_language": tm.get("lib_language", language),
        "impl": tm.get("impl", ""),
        "ffi_to": tm.get("ffi_to"),
        "concurrency": tm.get("concurrency", ""),
    }


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
        # Identity by reference. The build's classification and versions live
        # once in the archive's `builds` map; repeating eleven fields on every
        # cell cost about a fifth of the file to say the same eight things over
        # and over.
        "build": build_of(cell)[0],
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
    (build, kind, transport, peers) sweep, a cell whose rate is beaten by a
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
        # Grouped by build, not by variant: two versions of one library are two
        # different sweeps, and comparing a cell against a different version's
        # curve would flag a real version-to-version change as an artifact.
        key = (r["build"], r["kind"], r["transport"], r.get("peers"))
        groups.setdefault(key, []).append(r)
    for rs in groups.values():
        rs.sort(key=lambda r: r["payload_bytes"])
        rates = [r["throughput"]["msgs_per_s"] for r in rs]
        for i, r in enumerate(rs):
            rate = rates[i]
            larger_max = max(rates[i + 1:], default=0.0)
            inverted = rate > 0 and larger_max > rate * (1 + margin)
            r.setdefault("stability", {})["inverted"] = bool(inverted)


def write_archive(
    docs: Path, run_id: str, date: str, hardware: dict, records: list, builds: dict
) -> str:
    hist = docs / "history"
    hist.mkdir(parents=True, exist_ok=True)
    fname = f"{date}-run.json"
    # schema 2: records reference a build id; `builds` resolves it. The map is
    # written into the archive rather than looked up in variants.json at read
    # time because an archive is immutable history and variants.json is current.
    # A run from before a version bump has to keep reporting the version that
    # actually ran, forever.
    run = {
        "schema": 2,
        "run_id": run_id,
        "date": date,
        "hardware": hardware,
        "builds": builds,
        "records": records,
    }
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


def main():
    ap = argparse.ArgumentParser(description="Render orchestrator output for the dashboard")
    ap.add_argument("--scratch", required=True, type=Path)
    ap.add_argument("--run-id", default=None)
    ap.add_argument("--date", default=None, help="archive date (default: run-id or UTC today)")
    ap.add_argument("--docs", default=REPO / "docs", type=Path)
    ap.add_argument("--hardware-cpu", default="unknown host")
    ap.add_argument("--hardware-note", default="")
    args = ap.parse_args()

    run_id = args.run_id or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    date = args.date or run_id
    cells = load_cells(args.scratch)
    records = [to_archive_record(c) for c in cells]
    builds = dict(build_of(c) for c in cells)
    flag_inversions(records)  # payload-monotonicity correctness check across the sweep
    hardware = {"cpu": args.hardware_cpu, "note": args.hardware_note}

    fname = write_archive(args.docs, run_id, date, hardware, records, builds)
    print(f"rendered {len(records)} records -> docs/history/{fname}, updated index.json")


if __name__ == "__main__":
    main()
