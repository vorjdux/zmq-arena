#!/usr/bin/env python3
"""Generate the run matrix as a payload-size sweep across all five kinds.

The payload sizes follow monocoque's own throughput benches
(MESSAGE_SIZES = [64, 256, 1024, 4096, 16384]) so the arena's sweep lines up with
the engine's native benchmark points. Each runnable target is swept over every
size it supports.

Message counts shrink as the payload grows, so a large-payload cell moves a
sane amount of data and finishes inside the orchestrator's time budget on a slow
host; msgs/s and MB/s are rates, so the count does not bias the comparison. The
duration-based kinds (pubsub, fanout, fanin) take no count: they run a fixed
window and the message total is the result.

Usage:
  python3 scripts/gen_matrix.py                 # writes matrix.linode.json
  python3 scripts/gen_matrix.py --sizes 64,1024 --out matrix.quick.json
"""

import argparse
import json
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Monocoque's throughput-bench size set.
DEFAULT_SIZES = [64, 256, 1024, 4096, 16384]

# Count-based kinds: messages per payload size. Larger payloads carry fewer
# messages so total bytes and wall time stay bounded.
#
# Warmup is discarded before the timed window for both kinds now: latency skips
# its warmup round-trips, and throughput's consumer drains the warmup prefix
# untimed before starting its steady-state clock (the target times the measured
# block itself and reports THROUGHPUT count elapsed). Latency gets a generous 50%
# to settle the tail -- too little warmup lets the first cold round-trips dominate
# p99.9, and too few samples make p99.9 itself noisy. Throughput reaches steady
# state fast, so a modest 10% discarded prefix is enough.
THROUGHPUT_MSGS = {64: 200000, 256: 150000, 1024: 100000, 4096: 50000, 16384: 20000}
LATENCY_MSGS = {64: 40000, 256: 40000, 1024: 30000, 4096: 20000, 16384: 20000}

# Peer counts for the duration-based kinds, and their window. A longer window
# averages out scheduling jitter on a shared core.
PUBSUB_PEERS = 8
FAN_PEERS = 4
DURATION_SECS = 3.0

ALL_FIVE = ["throughput", "latency", "pubsub", "fanout", "fanin"]

# Per-target binary, knobs, and supported kinds. zmq.rs cannot fan-out or fan-in
# (its PUSH/PULL does not multiplex multiple peers on the bound side), so it runs
# only the first three kinds.
TARGETS = [
    {
        "id": "libzmq",
        "binary": "targets/libzmq_cpp_target/build/libzmq_target",
        "count_knobs": {"sndhwm": "1000", "rcvhwm": "1000", "io_threads": "1"},
        "mp_knobs": {"io_threads": "1"},
        "kinds": ALL_FIVE,
    },
    {
        "id": "monocoque",
        "binary": "targets/monocoque_target/target/release/monocoque-target",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
    {
        # Same engine, tokio (epoll) runtime. Its own binary because monocoque
        # picks the runtime at compile time; the (id, variant) pair keys the
        # dashboard series monocoque_tokio.
        "id": "monocoque",
        "binary": "targets/monocoque_target/target-tokio/release/monocoque-target",
        "variant": "tokio",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
    {
        "id": "rust_zmq",
        "binary": "targets/rust_zmq_target/target/release/rust-zmq-target",
        "count_knobs": {"sndhwm": "1000", "rcvhwm": "1000", "io_threads": "1"},
        "mp_knobs": {"io_threads": "1"},
        "kinds": ALL_FIVE,
    },
    {
        "id": "zeromq_rs",
        "binary": "targets/zeromq_rs_target/target/release/zeromq-rs-target",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ["throughput", "latency", "pubsub"],
    },
    {
        "id": "omq_tokio",
        "binary": "targets/omq_tokio_target/target/release/omq-tokio-target",
        "variant": "default",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
    {
        # Same binary, multi-thread tokio runtime. The (id, variant) pair keys the
        # dashboard series omq_tokio_mt.
        "id": "omq_tokio",
        "binary": "targets/omq_tokio_target/target/release/omq-tokio-target",
        "variant": "multi_thread",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
    {
        "id": "omq_compio",
        "binary": "targets/omq_compio_target/target/release/omq-compio-target",
        "variant": "default",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
]

ISOLATION = {"cpuset_cpus": "0", "cpuset_mems": "0", "memory_max_bytes": 268435456}

# Replication policy recorded in the matrix so a run is reproducible from the file
# alone. Each cell is measured at least min_replicates times as fresh process
# pairs, interleaved across cells; the adaptive loop stops a cell early once its
# primary metric's relative IQR falls to target_rel_iqr, and always by
# max_replicates. Outliers beyond mad_k scaled-MAD from the median are rejected
# before the reported median is taken -- but if more than max_outlier_frac of the
# draws have to be rejected the cell is flagged unstable rather than trusted, so a
# bimodal cell cannot look solid just because the filter kept one mode. These
# mirror the orchestrator defaults; a quick local run can shrink the counts with
# `zmq-arena run --replicates N`.
REPLICATION = {
    "min_replicates": 5,
    "max_replicates": 11,
    "warmup_replicates": 1,
    "target_rel_iqr": 0.05,
    "mad_k": 3.0,
    "max_outlier_frac": 0.25,
}


def target_spec(target, knobs_key):
    spec = {"id": target["id"], "binary": target["binary"], "knobs": target[knobs_key]}
    if target.get("variant"):
        spec["variant"] = target["variant"]
    return spec


def count_cell(target, kind, transport, size, msgs):
    # Both kinds discard warmup before timing: 50% for latency (to settle the
    # tail), a modest 10% prefix for throughput (drained before the steady clock).
    warmup = msgs // 2 if kind == "latency" else msgs // 10
    return {
        "target": target_spec(target, "count_knobs"),
        "transport": transport,
        "kind": kind,
        "payload_bytes": size,
        "messages": msgs,
        "warmup_messages": warmup,
    }


def duration_cell(target, kind, size, peers):
    return {
        "target": target_spec(target, "mp_knobs"),
        "transport": "tcp_netns",
        "kind": kind,
        "peers": peers,
        "duration_secs": DURATION_SECS,
        "payload_bytes": size,
        "messages": 0,
        "warmup_messages": 0,
    }


def build(sizes):
    entries = []
    for target in TARGETS:
        for size in sizes:
            if "throughput" in target["kinds"]:
                for transport in ("ipc", "tcp_netns"):
                    entries.append(count_cell(target, "throughput", transport, size, THROUGHPUT_MSGS[size]))
            if "latency" in target["kinds"]:
                for transport in ("ipc", "tcp_netns"):
                    entries.append(count_cell(target, "latency", transport, size, LATENCY_MSGS[size]))
            if "pubsub" in target["kinds"]:
                entries.append(duration_cell(target, "pubsub", size, PUBSUB_PEERS))
            if "fanout" in target["kinds"]:
                entries.append(duration_cell(target, "fanout", size, FAN_PEERS))
            if "fanin" in target["kinds"]:
                entries.append(duration_cell(target, "fanin", size, FAN_PEERS))
    return entries


def main():
    ap = argparse.ArgumentParser(description="Generate the payload-sweep run matrix")
    ap.add_argument("--sizes", default=",".join(map(str, DEFAULT_SIZES)),
                    help="comma-separated payload sizes in bytes")
    ap.add_argument("--out", default=str(REPO / "matrix.linode.json"), type=Path)
    args = ap.parse_args()

    sizes = [int(s) for s in args.sizes.split(",") if s]
    entries = build(sizes)
    doc = {
        "_comment": (
            f"Generated by scripts/gen_matrix.py. Payload sweep over {sizes} bytes "
            f"(monocoque's throughput-bench size set) across all five kinds per target. "
            f"Count-based kinds shrink their message count as the payload grows so cells "
            f"stay within budget; duration-based kinds run a {DURATION_SECS}s window. "
            f"{len(entries)} cells. On one vCPU the producer and consumer share the core, "
            f"so these numbers validate mechanics and the payload trend, not absolute "
            f"engine performance. Regenerate with: python3 scripts/gen_matrix.py"
        ),
        "isolation": ISOLATION,
        "replication": REPLICATION,
        "entries": entries,
    }
    args.out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"wrote {len(entries)} cells across {len(sizes)} sizes -> {args.out}")


if __name__ == "__main__":
    main()
