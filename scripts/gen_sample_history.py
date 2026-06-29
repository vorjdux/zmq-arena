#!/usr/bin/env python3
"""Generate illustrative SAMPLE history runs for the dashboard.

Synthetic numbers, not measurements. They exist so docs/index.html renders
something interactive before the first real weekly grid. Output goes to
docs/sample/ flagged sample=true. Real runs live in docs/history/ and use the
same schema.

The data model mirrors the omq.rs comparison harness:
  - a measured series is a VARIANT (engine + runtime), e.g. omq-tokio vs
    omq-tokio-mt are the same engine with different runtimes;
  - each variant is tagged with categories (engine / io model / threading) so
    the dashboard can compare any subset or group by category;
  - benchmark KINDS: throughput (PUSH/PULL), latency (REQ/REP), pubsub, fanout,
    fanin, each across transports and message sizes, with peer counts where
    applicable.

Run: python3 scripts/gen_sample_history.py
"""

import json
import math
import random
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "docs" / "sample"

RUN_DATES = [
    "2026-05-04", "2026-05-11", "2026-05-18",
    "2026-05-25", "2026-06-01", "2026-06-08",
]

SIZES = [64, 1024, 16384]
PUBSUB_PEERS = [1, 8, 64]
FAN_PEERS = [2, 4, 8]

# Variant registry. `lat`/`trend` shape the synthetic latency; `io`/`threading`/
# `engine` are category tags; `transports` and `pubsub` mirror the real per-impl
# support from run_comparisons.py. `since` delays a variant's first appearance.
VARIANTS = {
    "libzmq":        {"engine": "libzmq",   "io": "epoll",    "threading": "native", "lat": 1.00, "trend": 1.000, "transports": ["ipc", "tcp_netns", "inproc"], "pubsub": True,  "since": 0},
    "zmq.rs":        {"engine": "zmq.rs",   "io": "epoll",    "threading": "multi",  "lat": 1.85, "trend": 0.995, "transports": ["ipc", "tcp_netns"],            "pubsub": True,  "since": 0},
    "omq_tokio":     {"engine": "omq",      "io": "epoll",    "threading": "single", "lat": 1.05, "trend": 0.965, "transports": ["ipc", "tcp_netns", "inproc"], "pubsub": True,  "since": 0},
    "omq_tokio_mt":  {"engine": "omq",      "io": "epoll",    "threading": "multi",  "lat": 1.12, "trend": 0.960, "transports": ["ipc", "tcp_netns", "inproc"], "pubsub": True,  "since": 0},
    "omq_compio":    {"engine": "omq",      "io": "io_uring", "threading": "multi",  "lat": 0.82, "trend": 0.955, "transports": ["ipc", "tcp_netns", "inproc"], "pubsub": True,  "since": 0},
    "omq_compio_st": {"engine": "omq",      "io": "io_uring", "threading": "single", "lat": 0.90, "trend": 0.958, "transports": ["inproc"],                      "pubsub": False, "since": 0},
    "rzmq":          {"engine": "rzmq",     "io": "io_uring", "threading": "multi",  "lat": 0.88, "trend": 0.975, "transports": ["ipc", "tcp_netns", "inproc"], "pubsub": True,  "since": 0},
    "celerity":      {"engine": "celerity", "io": "epoll",    "threading": "multi",  "lat": 1.60, "trend": 0.930, "transports": ["ipc", "tcp_netns"],            "pubsub": True,  "since": 2},
    "monocoque":     {"engine": "monocoque","io": "io_uring", "threading": "single", "lat": 0.95, "trend": 0.945, "transports": ["ipc", "tcp_netns", "inproc"], "pubsub": True,  "since": 3},
}

# Base p50 latency (ns) for a 64B payload, per transport. inproc is in-process
# (single process, the one exception to the arena's isolation rule) and fastest.
BASE_P50_NS = {"inproc": 700.0, "ipc": 2100.0, "tcp_netns": 8200.0}


def payload_factor(payload):
    return 1.0 + math.log2(payload / 64) * 0.12


def latency_block(rng, v, transport, payload, run_idx, extra=1.0):
    p50 = (
        BASE_P50_NS[transport] * v["lat"] * payload_factor(payload)
        * (v["trend"] ** run_idx) * extra * rng.uniform(0.97, 1.03)
    )
    tail = 2.6 if v["io"] == "io_uring" else 3.4
    p90 = p50 * rng.uniform(1.25, 1.45)
    p99 = p50 * rng.uniform(tail * 0.7, tail)
    p999 = p99 * rng.uniform(1.6, 2.4)
    mn = p50 * rng.uniform(0.55, 0.7)
    mx = p999 * rng.uniform(1.3, 2.0)
    return {"min": round(mn), "p50": round(p50), "p90": round(p90),
            "p99": round(p99), "p999": round(p999), "max": round(mx)}


def throughput_block(rng, v, transport, payload, run_idx, peers=1):
    p50 = BASE_P50_NS[transport] * v["lat"] * payload_factor(payload) * (v["trend"] ** run_idx)
    base_msgs = (1.0e9 / p50) * rng.uniform(0.45, 0.6)
    msgs_s = base_msgs / (1.0 + math.log2(payload / 64) * 0.18)
    msgs_s *= peers  # aggregate across subscribers / pushers
    mbps = msgs_s * payload / 1e6
    return {"msgs_per_s": round(msgs_s, 1), "mbps": round(mbps, 2)}


def telemetry(rng, v, payload):
    msgs = 1_000_000
    if v["io"] == "io_uring":
        sysc = {"epoll_wait": 0, "epoll_ctl": 4, "sendmsg": 0, "recvmsg": 0,
                "io_uring_enter": int(msgs / rng.uniform(28, 40))}
    else:
        sysc = {"epoll_wait": int(msgs / rng.uniform(1.4, 2.2)),
                "epoll_ctl": int(rng.uniform(8, 40)),
                "sendmsg": int(msgs / rng.uniform(1.0, 1.6)),
                "recvmsg": int(msgs / rng.uniform(1.0, 1.6)),
                "io_uring_enter": 0}
    sched = {"voluntary": int(rng.uniform(500, 4000)),
             "involuntary": int(rng.uniform(0, 18))}
    mem = int(rng.uniform(6, 30) * 1e6) + payload * 64
    cpu = round(rng.uniform(0.6, 2.4), 6)
    return sysc, sched, mem, cpu


def base_record(vid, v, kind, transport, payload, peers):
    return {
        "variant": vid, "engine": v["engine"], "io": v["io"],
        "threading": v["threading"], "kind": kind, "transport": transport,
        "payload_bytes": payload, "peers": peers,
    }


def build_records(rng, run_idx):
    out = []
    for vid, v in VARIANTS.items():
        if run_idx < v["since"]:
            continue
        for transport in v["transports"]:
            # throughput (PUSH/PULL) + latency (REQ/REP)
            for payload in SIZES:
                sysc, sched, mem, cpu = telemetry(rng, v, payload)
                tr = base_record(vid, v, "throughput", transport, payload, None)
                tr.update({"latency_ns": None,
                           "throughput": throughput_block(rng, v, transport, payload, run_idx),
                           "cpu_seconds": cpu, "syscalls": sysc, "sched": sched,
                           "peak_memory_bytes": mem})
                out.append(tr)

                sysc, sched, mem, cpu = telemetry(rng, v, payload)
                la = base_record(vid, v, "latency", transport, payload, None)
                la.update({"latency_ns": latency_block(rng, v, transport, payload, run_idx),
                           "throughput": None, "cpu_seconds": cpu,
                           "syscalls": sysc, "sched": sched, "peak_memory_bytes": mem})
                out.append(la)

            # pub/sub throughput (peer counts), if supported, non-inproc
            if v["pubsub"] and transport in ("ipc", "tcp_netns"):
                for peers in PUBSUB_PEERS:
                    for payload in SIZES:
                        sysc, sched, mem, cpu = telemetry(rng, v, payload)
                        ps = base_record(vid, v, "pubsub", transport, payload, peers)
                        ps.update({"latency_ns": None,
                                   "throughput": throughput_block(rng, v, transport, payload, run_idx, peers),
                                   "cpu_seconds": cpu, "syscalls": sysc, "sched": sched,
                                   "peak_memory_bytes": mem})
                        out.append(ps)

        # fan-out and fan-in: TCP only (matches omq)
        if "tcp_netns" in v["transports"]:
            for kind in ("fanout", "fanin"):
                for peers in FAN_PEERS:
                    for payload in SIZES:
                        sysc, sched, mem, cpu = telemetry(rng, v, payload)
                        fr = base_record(vid, v, kind, "tcp_netns", payload, peers)
                        fr.update({"latency_ns": None,
                                   "throughput": throughput_block(rng, v, "tcp_netns", payload, run_idx, peers),
                                   "cpu_seconds": cpu, "syscalls": sysc, "sched": sched,
                                   "peak_memory_bytes": mem})
                        out.append(fr)
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    # clear old sample run files
    for f in OUT.glob("*-run.json"):
        f.unlink()
    manifest_runs = []
    for run_idx, date in enumerate(RUN_DATES):
        rng = random.Random(f"{date}-seed-v2")
        run = {
            "run_id": date, "date": date, "sample": True,
            "hardware": {"cpu": "AMD EPYC 9554P (bare metal)",
                         "note": "turbo off, C-states locked, performance governor"},
            "records": build_records(rng, run_idx),
        }
        fname = f"{date}-run.json"
        (OUT / fname).write_text(json.dumps(run, separators=(",", ":")))
        manifest_runs.append({"date": date, "file": fname})

    manifest = {"schema": 2, "sample": True,
                "note": "Synthetic sample data for dashboard preview. Not measurements.",
                "runs": manifest_runs}
    (OUT / "index.json").write_text(json.dumps(manifest, indent=2))
    tot = sum(len(json.loads((OUT / r["file"]).read_text())["records"]) for r in manifest_runs)
    print(f"wrote {len(manifest_runs)} runs, {tot} records to {OUT}")


if __name__ == "__main__":
    main()
