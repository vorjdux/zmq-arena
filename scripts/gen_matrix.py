#!/usr/bin/env python3
"""Generate the run matrix as a two-tier payload sweep.

The matrix is tiered, following the policy the OMQ maintainer settled on for
`omq.rs` and for the same reason: benchmarking every implementation across every
pattern is neither maintainable nor fair. An implementation that never intended
to multiplex many peers on a bound socket should not be scored on fan-out.

  HEADLINE  tcp only, the three patterns every implementation here genuinely
            implements: REQ/REP latency, 1-to-1 PUSH/PULL throughput, and
            PUB/SUB at 32 subscribers. These are the cross-library charts.

  EXTENDED  ipc, fan-out and fan-in.

A variant is an engine plus a runtime, and every runtime an engine ships gets
its own variant: monocoque builds on compio/tokio/smol, zmq.rs on
tokio/async-std/async-dispatcher, omq runs current-thread, multi-thread and its
synchronous blocking API. Benchmarking only some of an engine's runtimes would
mean picking which of its configurations is allowed to represent it.

The tiers split by PATTERN, never by library. Every implementation runs every
cell in both tiers that it is capable of running, and the only thing that ever
excludes one is a documented inability to serve that pattern (zmq.rs cannot
multiplex several peers on a bound PUSH/PULL, so it has no fan-out or fan-in).
A tier whose membership is a list of favoured names would not be a benchmark,
it would be an advertisement, and the ranking maths would quietly reward the
libraries that were let into the extra cells.

Both tiers sweep the payload over [64, 256, 1024, 4096, 16384]. That is the size
set the OMQ comparison and monocoque's own throughput benches both sweep, so the
arena's points line up with the numbers those projects publish rather than being
a set invented here.

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
# 16 B is here on purpose: libzmq keeps messages up to ~33 bytes inline in the
# message struct and OMQ up to ~55, so a sweep that starts at 64 never exercises
# either small-message path and the engines look more alike at the small end than
# they are. Every size is a power of two and each is 4x the last, which also
# keeps the log axis honest.
DEFAULT_SIZES = [16, 64, 256, 1024, 4096, 16384]

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
THROUGHPUT_MSGS = {16: 200000, 64: 200000, 256: 150000, 1024: 100000, 4096: 50000, 16384: 20000}
LATENCY_MSGS = {16: 40000, 64: 40000, 256: 40000, 1024: 30000, 4096: 20000, 16384: 20000}

# Peer counts for the duration-based kinds, and their window. A longer window
# averages out scheduling jitter on a shared core.
#
# PUB/SUB runs at 32 subscribers to match the OMQ headline chart, so the two
# projects' pub/sub numbers describe the same shape of workload. It is the
# expensive cell in the grid: one process per subscriber means 33 processes in
# the cpuset, which is the point (a broadcast to 32 real peers is what separates
# the engines) but is also why it is a headline cell and not swept elsewhere.
PUBSUB_PEERS = 32
FAN_PEERS = 4
DURATION_SECS = 3.0

# The three patterns every implementation here actually supports, over tcp.
HEADLINE_KINDS = ["latency", "throughput", "pubsub"]
# Coverage tier: the fan patterns, plus the two count-based kinds over ipc.
EXTENDED_KINDS = ["fanout", "fanin"]
IPC_KINDS = ["throughput", "latency"]

ALL_FIVE = ["throughput", "latency", "pubsub", "fanout", "fanin"]

# Per-target binary, knobs, and supported kinds.
#
# `kinds` is a capability declaration, not a preference: it lists what the
# implementation can actually do, and it is the ONLY thing that decides which
# cells it runs. Shortening it to save grid time would silently remove a library
# from comparisons it would otherwise have appeared in, so it must reflect the
# engine's API and nothing else.
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
        # PUB fans out from a worker pool that defaults to the host CPU count
        # clamped to [2, 16]. Pinning it to 1 keeps the cell's process count a
        # property of the matrix rather than of whatever machine ran it.
        "mp_knobs": {"pub_workers": "1"},
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
        "mp_knobs": {"pub_workers": "1"},
        "kinds": ALL_FIVE,
    },
    {
        # Third runtime the engine ships (smol, epoll via polling).
        "id": "monocoque",
        "binary": "targets/monocoque_target/target-smol/release/monocoque-target",
        "variant": "smol",
        "count_knobs": {},
        "mp_knobs": {"pub_workers": "1"},
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
        # rzmq, epoll backend. The engine ships two IO backends and both are
        # measured; the io_uring one is the entry below.
        "id": "rzmq",
        "binary": "targets/rzmq_target/target/release/rzmq-target",
        "variant": "default",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
    {
        # Same binary, io_uring session with zero-copy send and multishot recv,
        # matching the rzmq peer in the omq.rs comparison harness.
        "id": "rzmq",
        "binary": "targets/rzmq_target/target/release/rzmq-target",
        "variant": "io_uring",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
    {
        # celerity implements PUB/SUB and REQ/REP only: the crate has no
        # pipeline core, so there is no PUSH/PULL to drive and the pipeline
        # kinds are simply not scheduled for it.
        "id": "celerity",
        "binary": "targets/celerity_target/target/release/celerity-target",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ["latency", "pubsub"],
    },
    {
        # tmq: Tokio bindings over libzmq (via rust-zmq). The engine is libzmq,
        # so it runs all five kinds; the series is here to isolate binding and
        # async-wrapper overhead against `libzmq` and `rust_zmq`, which reach the
        # same engine differently.
        "id": "tmq",
        "binary": "targets/tmq_target/target/release/tmq-target",
        "count_knobs": {},
        "mp_knobs": {},
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
        # zmq.rs also picks its runtime by feature, so its other two shipped
        # runtimes are separate builds and separate series.
        "id": "zeromq_rs",
        "binary": "targets/zeromq_rs_target/target-async-std/release/zeromq-rs-target",
        "variant": "async_std",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ["throughput", "latency", "pubsub"],
    },
    {
        "id": "zeromq_rs",
        "binary": "targets/zeromq_rs_target/target-async-dispatcher/release/zeromq-rs-target",
        "variant": "async_dispatcher",
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
        # omq's third execution model: the synchronous API over library-owned IO
        # threads, selected at run time rather than by a separate build. This is
        # the model libzmq uses, so the pair is a direct comparison of two
        # implementations of the same idea.
        "id": "omq_tokio",
        "binary": "targets/omq_tokio_target/target/release/omq-tokio-target",
        "variant": "blocking",
        "count_knobs": {},
        "mp_knobs": {},
        "kinds": ALL_FIVE,
    },
]

# Four cores per cell so the producer and consumer run on separate cores instead
# of time-sharing one. On a single core the processes contend for CPU and that
# contention, not the library, is what the numbers measure, which flattens real
# differences and hides per-workload tuning. The host must have at least 4 cores
# in this cpuset.
#
# The 32-subscriber pub/sub cell necessarily oversubscribes this: 33 processes on
# 4 cores. That is inherent to the workload rather than a flaw in the isolation --
# the cell measures aggregate broadcast throughput out of one publisher, which is
# the quantity the OMQ chart reports too, and every implementation meets the same
# oversubscription. It is not a per-subscriber latency measurement and must not be
# read as one.
ISOLATION = {"cpuset_cpus": "0-3", "cpuset_mems": "0", "memory_max_bytes": 268435456}

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


def count_msgs(kind, size):
    return (LATENCY_MSGS if kind == "latency" else THROUGHPUT_MSGS)[size]


def headline(sizes):
    """Tcp, the three universally supported patterns, for every target that
    supports them (which today is all of them)."""
    entries = []
    for target in TARGETS:
        for size in sizes:
            for kind in HEADLINE_KINDS:
                if kind not in target["kinds"]:
                    continue
                if kind == "pubsub":
                    entries.append(duration_cell(target, "pubsub", size, PUBSUB_PEERS))
                else:
                    entries.append(
                        count_cell(target, kind, "tcp_netns", size, count_msgs(kind, size))
                    )
    return entries


def extended(sizes):
    """Ipc and the fan patterns, for every target that supports them.

    Membership is capability only. A target appears in every extended cell its
    `kinds` allows, exactly like every other target.
    """
    entries = []
    for target in TARGETS:
        for size in sizes:
            for kind in IPC_KINDS:
                if kind in target["kinds"]:
                    entries.append(
                        count_cell(target, kind, "ipc", size, count_msgs(kind, size))
                    )
            for kind in EXTENDED_KINDS:
                if kind in target["kinds"]:
                    entries.append(duration_cell(target, kind, size, FAN_PEERS))
    return entries


def build(sizes):
    return headline(sizes) + extended(sizes)


def main():
    ap = argparse.ArgumentParser(description="Generate the payload-sweep run matrix")
    ap.add_argument("--sizes", default=",".join(map(str, DEFAULT_SIZES)),
                    help="comma-separated payload sizes in bytes")
    ap.add_argument("--out", default=str(REPO / "matrix.linode.json"), type=Path)
    args = ap.parse_args()

    sizes = [int(s) for s in args.sizes.split(",") if s]
    entries = build(sizes)
    n_head, n_ext = len(headline(sizes)), len(extended(sizes))
    doc = {
        "_comment": (
            f"Generated by scripts/gen_matrix.py. Two tiers, both swept over {sizes} "
            f"bytes (the size set the OMQ comparison and monocoque's benches both "
            f"sweep). Tiers split by PATTERN, never by library: every implementation "
            f"runs every cell it is capable of. HEADLINE: tcp, the three patterns "
            f"every implementation here implements "
            f"(REQ/REP latency, 1-to-1 PUSH/PULL throughput, PUB/SUB at "
            f"{PUBSUB_PEERS} subscribers) -- these are the cross-library charts. "
            f"EXTENDED: ipc, fan-out and fan-in, for every implementation able to "
            f"run them (zmq.rs has no fan-out or fan-in: its PUSH/PULL does not "
            f"multiplex several peers on the bound side). "
            f"{len(headline(sizes))} headline + {len(extended(sizes))} "
            f"extended = {len(entries)} cells. Count-based kinds shrink their message "
            f"count as the payload grows so cells stay within budget; duration-based "
            f"kinds run a {DURATION_SECS}s window. Each cell runs in a 4-core cpuset "
            f"so the producer and consumer do not time-share one core; the numbers "
            f"still come from a shared host, so treat them as the payload trend and "
            f"relative shape, not a final absolute verdict. Regenerate with: "
            f"python3 scripts/gen_matrix.py"
        ),
        "isolation": ISOLATION,
        "replication": REPLICATION,
        "entries": entries,
    }
    args.out.write_text(json.dumps(doc, indent=2) + "\n")
    print(
        f"wrote {len(entries)} cells ({n_head} headline + {n_ext} extended) "
        f"across {len(sizes)} sizes -> {args.out}"
    )


if __name__ == "__main__":
    main()
