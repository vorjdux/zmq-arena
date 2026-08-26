#!/usr/bin/env python3
"""zmq-arena target wrapper for the Python ZeroMQ bindings.

One wrapper, two engines. `--variant pyzmq` uses pyzmq, the Cython binding to
libzmq; `--variant pyomq` uses pyomq, which presents the same API over the omq
Rust core. Same language, same socket calls, different engine underneath, which
is the comparison a Python user actually faces.

The wrapper times its own steady-state window and prints one result line. It
does not let the harness time the process: a Python interpreter spends 50-150ms
importing before it reaches the benchmark, and folding that into the measurement
would read as the language being slow at messaging rather than slow to start.
"""
import argparse
import json
import os
import sys
import time


def load_engine(variant):
    """Return (module, engine name, version). The two bindings expose the same
    surface, so everything below is written once against whichever is loaded."""
    if variant in ("pyomq", "omq"):
        import pyomq as z  # noqa: F401  drop-in replacement for the pyzmq API
        return z, "omq", getattr(z, "__version__", "unknown")
    import zmq as z
    return z, "libzmq", getattr(z, "__version__", "unknown")


def describe(variant):
    z, engine, ver = load_engine(variant)
    if engine == "libzmq":
        lib = ".".join(str(p) for p in z.zmq_version_info())
        binding, lib_lang, ffi_to = ver, "C++", "C"
    else:
        # pyomq binds the Rust core, so the engine language is Rust and the
        # version we can read is the binding's.
        lib, binding, lib_lang, ffi_to = ver, ver, "Rust", "Rust"
    print(json.dumps({
        "engine": engine,
        "lib_version": lib,
        "binding_version": binding,
        "lib_language": lib_lang,
        "impl": "ffi",
        "ffi_to": ffi_to,
        "language": "Python",
        "concurrency": "sync",
        "threading": "native",
        "io": "epoll",
    }, separators=(",", ":")))


def knob(knobs, key, default=None):
    for kv in knobs:
        k, _, v = kv.partition("=")
        if k == key:
            return v
    env = os.environ.get("ARENA_KNOB_" + key.upper())
    return env if env is not None else default


def context(z, knobs):
    ctx = z.Context()
    io_threads = knob(knobs, "io_threads")
    if io_threads:
        if hasattr(z, "IO_THREADS"):
            # Set before any socket exists, or libzmq ignores it.
            ctx.set(z.IO_THREADS, int(io_threads))
        else:
            # pyomq 0.20.1 exposes no IO_THREADS, so the arena's one-lane rule
            # cannot be applied to it. Say so on stderr rather than silently
            # letting one engine size its own IO pool while the others are
            # pinned: that is the asymmetry the rule exists to prevent.
            print("pyzmq-target: WARNING this engine exposes no IO_THREADS; "
                  "the io_threads knob is not applied", file=sys.stderr)
    return ctx


def apply_hwm(z, sock, knobs):
    for key, opt in (("sndhwm", "SNDHWM"), ("rcvhwm", "RCVHWM")):
        v = knob(knobs, key)
        if v and hasattr(z, opt):
            sock.setsockopt(getattr(z, opt), int(v))


def percentiles(rtts):
    if not rtts:
        return "LATENCY 0 0 0 0 0 0 0"
    rtts.sort()
    def q(p):
        return rtts[min(int(len(rtts) * p), len(rtts) - 1)]
    return "LATENCY {} {} {} {} {} {} {}".format(
        len(rtts), rtts[0], q(0.50), q(0.90), q(0.99), q(0.999), rtts[-1])


def run_throughput(z, ctx, a, payload):
    if a.role == "sub":
        s = ctx.socket(z.PULL)
        apply_hwm(z, s, a.knob)
        s.bind(a.endpoint)
        total = a.messages + a.warmup
        # Drain warmup before starting the clock, so the connection handshake
        # and the ramp are outside the measured window.
        for _ in range(a.warmup):
            s.recv()
        t0 = time.perf_counter()
        for _ in range(a.messages):
            s.recv()
        secs = max(time.perf_counter() - t0, 1e-9)
        print("THROUGHPUT {} {:.6f}".format(a.messages, secs))
        del total
    else:
        s = ctx.socket(z.PUSH)
        apply_hwm(z, s, a.knob)
        s.connect(a.endpoint)
        for _ in range(a.messages + a.warmup):
            s.send(payload)
        # Let queued frames drain rather than dropping them on close, which
        # would break the "no data dropping" rule.
        s.close(linger=-1)


def run_latency(z, ctx, a, payload):
    if a.role == "sub":
        s = ctx.socket(z.REP)
        s.bind(a.endpoint)
        try:
            while True:
                s.send(s.recv())
        except Exception:
            pass  # the REQ side went away: the normal end of the cell
    else:
        s = ctx.socket(z.REQ)
        s.connect(a.endpoint)
        for _ in range(a.warmup):
            s.send(payload)
            s.recv()
        rtts = []
        for _ in range(a.messages):
            t = time.perf_counter_ns()
            s.send(payload)
            s.recv()
            rtts.append(time.perf_counter_ns() - t)
        print(percentiles(rtts))


def run_pubsub(z, ctx, a, payload):
    if a.role == "pub":
        s = ctx.socket(z.PUB)
        apply_hwm(z, s, a.knob)
        s.bind(a.endpoint)
        # PUB drops what it sends before a subscriber has finished subscribing,
        # so an unconditional warm-up would measure the drop path. There is no
        # portable readiness signal here, so settle briefly and then publish.
        time.sleep(0.5)
        while True:
            s.send(payload)
    else:
        s = ctx.socket(z.SUB)
        apply_hwm(z, s, a.knob)
        s.setsockopt(z.SUBSCRIBE, b"")
        s.connect(a.endpoint)
        s.recv()  # first message: subscription is live, start the clock after it
        count, t0 = 1, time.perf_counter()
        deadline = t0 + a.duration_secs
        while time.perf_counter() < deadline:
            s.recv()
            count += 1
        print("THROUGHPUT {} {:.6f}".format(count, max(time.perf_counter() - t0, 1e-9)))


def run_pipeline(z, ctx, a, payload):
    """fanout: one PUSH to N PULL. fanin: N PUSH to one PULL."""
    sock_type = z.PUSH if a.role == "pub" else z.PULL
    s = ctx.socket(sock_type)
    apply_hwm(z, s, a.knob)
    if a.bind:
        s.bind(a.endpoint)
    else:
        s.connect(a.endpoint)
    if a.role == "pub":
        while True:
            s.send(payload)
    else:
        s.recv()
        count, t0 = 1, time.perf_counter()
        deadline = t0 + a.duration_secs
        while time.perf_counter() < deadline:
            s.recv()
            count += 1
        print("THROUGHPUT {} {:.6f}".format(count, max(time.perf_counter() - t0, 1e-9)))


def main():
    variant = "pyzmq"
    if "--variant" in sys.argv:
        variant = sys.argv[sys.argv.index("--variant") + 1]
    # describe is a fast path before argument parsing, per the target contract.
    if len(sys.argv) > 1 and sys.argv[1] == "describe":
        describe(variant)
        return 0

    p = argparse.ArgumentParser()
    p.add_argument("--role", required=True, choices=["pub", "sub"])
    p.add_argument("--kind", default="throughput")
    p.add_argument("--transport", required=True)
    p.add_argument("--endpoint", required=True)
    p.add_argument("--payload-bytes", dest="payload_bytes", type=int, required=True)
    p.add_argument("--messages", type=int, required=True)
    p.add_argument("--warmup", type=int, default=0)
    p.add_argument("--peers", type=int, default=None)
    p.add_argument("--variant", default="pyzmq")
    p.add_argument("--bind", action="store_true")
    p.add_argument("--duration-secs", dest="duration_secs", type=float, default=0.0)
    p.add_argument("--knob", dest="knob", action="append", default=[])
    a = p.parse_args()

    z, engine, _ = load_engine(a.variant)
    print("pyzmq-target: engine={} role={} kind={} ep={} payload={}B msgs={}".format(
        engine, a.role, a.kind, a.endpoint, a.payload_bytes, a.messages), file=sys.stderr)

    ctx = context(z, a.knob)
    payload = b"x" * a.payload_bytes
    try:
        if a.kind == "throughput":
            run_throughput(z, ctx, a, payload)
        elif a.kind == "latency":
            run_latency(z, ctx, a, payload)
        elif a.kind == "pubsub":
            run_pubsub(z, ctx, a, payload)
        elif a.kind in ("fanout", "fanin"):
            run_pipeline(z, ctx, a, payload)
        else:
            raise SystemExit("unsupported kind: " + a.kind)
    finally:
        sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
