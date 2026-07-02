<p align="center">
  <img src="docs/zmq-arena-logo.svg" alt="zmq-arena: ZMTP benchmark battleground" width="520">
</p>

# zmq-arena

A benchmarking harness for ZMTP (the ZeroMQ wire protocol). It runs several
implementations through the same isolated, instrumented conditions and records
how each one does, so the comparison is about the implementations and not about
the harness.

The repo is split in two. A Rust control plane (the orchestrator) owns isolation
and measurement. Independent data-plane binaries (the targets) own the socket
code and its tuning. The only thing they share is a command-line contract, so a
target can be written in any language and still take part.

Status: work in progress. The data and reporting side is real and tested. The
measurement side is partly real: process isolation, a throughput run path, and
scheduler/CPU/memory capture work; netns, eBPF syscall counting, latency and
pub/sub measurement, and most of the engine socket loops are not wired yet. See
the status table near the end.

## Why the targets are not one Cargo workspace

Each target under `targets/` is its own project with its own `Cargo.toml`,
`Cargo.lock`, release profile, and toolchain pin. They are deliberately not
members of the orchestrator's workspace.

A single workspace resolves one dependency graph, one lockfile, one release
profile, and one toolchain across every member. An implementation would then be
measured against whatever versions and features the resolver settled on, not the
ones it actually ships with. For a comparison benchmark that is a thumb on the
scale. Keeping each target standalone lets `zeromq-rs` pin its own tokio,
`monocoque` set its own LTO, and a future Go or C target build with its native
toolchain, none of them touching each other. This follows the omq.rs comparison
harness, where each bench peer is a separate build unit.

## Targets

| directory | engine | language | crate or source | model |
|-----------|--------|----------|-----------------|-------|
| `libzmq_cpp_target` | libzmq | C++ | system `libzmq` via CMake | epoll, the reference |
| `rust_zmq_target` | libzmq | Rust | `zmq = "0.10"` (rust-zmq) | epoll, FFI binding over libzmq |
| `zeromq_rs_target` | zmq.rs | Rust | `zeromq = "0.6"` | epoll + tokio |
| `omq_tokio_target` | omq-tokio | Rust | git `paddor/omq.rs` | mio, tokio |
| `omq_compio_target` | omq-compio | Rust | git `paddor/omq.rs` | io_uring, single-thread, Linux 6.0+ |
| `rzmq_target` | rzmq | Rust | `rzmq = "0.5.21"` | io_uring + TCP_CORK, Linux |
| `celerity_target` | celerity | Rust | `celerity = "0.2.0"` | sans-IO ZMTP 3.1 + tokio |
| `monocoque_target` | monocoque | Rust | `monocoque-rs = "0.1.5"` | io_uring/compio, ZMTP 3.1 |

libzmq, rust-zmq, and monocoque run all five kinds. rust-zmq is the same C core
as libzmq reached through the Rust binding, so the pair measures binding
overhead. The two omq backends, omq-tokio (epoll/mio) and omq-compio (io_uring),
run all five over the omq `Socket` API; omq-tokio also exposes a multi-thread
runtime as a second variant. zmq.rs runs throughput, latency, and pub/sub; it
does not run fan-out or fan-in, because its PUSH/PULL sockets do not round-robin
or fair-queue across multiple peers on the bound side. rzmq and celerity remain
stubs until each is written against its engine's API. Crate identities and
versions are verified against crates.io and the upstream repos. See
`targets/README.md` for the command-line contract and how to add a target.

omq's PUSH does strict round-robin with HWM backpressure rather than libzmq-style
load-balancing to any ready peer, so its fan-out underperforms on a single shared
core (a lagging consumer gates the rotation). On bare metal, where each consumer
has its own core, that stall does not occur. The other four kinds reach the
millions of msgs/s on the dev host.

## Benchmarks and variants

The harness runs the same set of benchmarks as the omq comparison: throughput
(PUSH/PULL), latency (REQ/REP), pub/sub, fan-out, and fan-in, over ipc, loopback
tcp, and inproc, across a payload sweep, with peer counts where they apply.

A measured series is a variant, meaning an engine plus a runtime, not just an
engine. One binary can expose several runtimes, so `omq-tokio` in its
current-thread and multi-thread modes shows up as two series you can compare
directly, alongside `omq-compio` on io_uring.

| variant | target | engine | io model | threading | selected by |
|---------|--------|--------|----------|-----------|-------------|
| `libzmq` | libzmq_cpp_target | libzmq | epoll | native threads | only variant |
| `rust_zmq` | rust_zmq_target | libzmq | epoll | native threads | only variant |
| `zmq.rs` | zeromq_rs_target | zmq.rs | epoll | tokio | only variant |
| `omq_tokio` | omq_tokio_target | omq | mio/epoll | current-thread | `--variant default` |
| `omq_tokio_mt` | omq_tokio_target | omq | mio/epoll | multi-thread | `--variant multi_thread` |
| `omq_compio` | omq_compio_target | omq | io_uring | single-thread | `--variant default` |
| `rzmq` | rzmq_target | rzmq | io_uring | tokio | only variant |
| `celerity` | celerity_target | celerity | epoll | tokio | only variant |
| `monocoque` | monocoque_target | monocoque | io_uring | compio | only variant |

Each record carries the variant's category tags (engine, io model, threading),
which is what lets the dashboard group and compare by category.

## Build

The orchestrator is a small workspace; build it on its own:

```
cargo build --release --manifest-path orchestrator/Cargo.toml
```

The libzmq target builds through CMake and links `libzmq` (install `libzmq3-dev`
or `zeromq-devel` first):

```
cmake -S targets/libzmq_cpp_target -B targets/libzmq_cpp_target/build -DCMAKE_BUILD_TYPE=Release
cmake --build targets/libzmq_cpp_target/build --parallel
```

The rust-zmq target links the same system `libzmq` through pkg-config, so it
needs `libzmq3-dev` (or `zeromq-devel`) as well:

```
cd targets/rust_zmq_target && cargo build --release
```

`scripts/build-targets.sh` builds the orchestrator and every target, each in its
own invocation so per-target lockfiles and toolchains are honored.

## Running it

A `Makefile` wraps the usual flow: `make build` compiles the control plane and
the runnable targets, `make run` runs the matrix and renders the result into
`docs/`, and `make` does both. `make run-root` runs under sudo so cgroup pinning
applies. `make help` lists the rest. The commands below are what those targets
run.

Show the expanded plan without spawning anything:

```
cargo run --release -p zmq-arena-orchestrator -- run --matrix matrix.example.json --dry-run
```

A real run provisions cgroups and needs root for full isolation:

```
sudo ./target/release/zmq-arena run --matrix matrix.example.json --run-id "$(date -u +%F)" --out scratch/
```

Each cell writes one JSON record. `scripts/render_results.py` turns a scratch
directory into the dashboard archive and `RANKING.md`.

### On a single-vCPU dev host (Linode)

A small VM is the right place to check the wiring before bare metal. On Ubuntu
24.04:

```
bash scripts/setup-ubuntu.sh
cargo build --release --manifest-path orchestrator/Cargo.toml
cmake -S targets/libzmq_cpp_target -B targets/libzmq_cpp_target/build -DCMAKE_BUILD_TYPE=Release
cmake --build targets/libzmq_cpp_target/build --parallel
./target/release/zmq-arena run --matrix matrix.linode.json --run-id "$(date -u +%F)" --out "scratch/$(date -u +%F)"
python3 scripts/render_results.py --scratch "scratch/$(date -u +%F)" --run-id "$(date -u +%F)"
```

`matrix.linode.json` is a payload sweep over 64, 256, 1024, 4096, and 16384 byte
messages (monocoque's own throughput-bench size set) across all five kinds and
every runnable target, so the dashboard's size-sweep view shows how each engine
trades off as the payload grows. It is generated by `scripts/gen_matrix.py`;
regenerate with `make matrix`, or pass `--sizes` for a lighter set. Count-based
cells carry fewer messages at larger payloads so they stay within budget; msgs/s
and MB/s are rates, so the count does not bias the comparison.

Telemetry is captured the same way for every cell: CPU and context switches from
`getrusage`, peak memory from the summed per-process `VmHWM`. cgroups are skipped
cleanly if you are not root; memory and CPU still record without them. Syscall
counts need perf, so they read zero unless you run under sudo (`make run-root`)
on a host with tracefs and `perf_event_paranoid <= 1`.

One thing a single-vCPU VM cannot tell you is comparative performance. The two
processes share the core, cpuset pinning does nothing, and a guest cannot lock
Turbo or C-states. Treat those numbers as a wiring check. Real tail latency needs
bare metal.

## Dashboard

The `docs/` pages are self-contained (Apache ECharts, no build step) and meant
for GitHub Pages with the source set to `docs/`. They read the run archives under
`docs/history/` and fall back to synthetic sample data under `docs/sample/` until
the first real run lands. A top nav links three pages.

`index.html` is the Overview: the landing page. It leads with the global ranking,
a leaderboard that averages each library's rank position across every benchmark
in the run (latency and throughput count equally) so one number says who is ahead
overall, with the mean CPU and memory footprint alongside. Below it is a grid of
small-multiple panels, one per scenario (throughput ipc, latency tcp, pub/sub,
fan-out, fan-in, ...), each plotting payload size against the metric with one line
per library. Every library keeps the same colour in every panel, so you can read
the whole comparison at a glance without picking. The library chips act as a
shared legend and filter; a segmented control switches the latency percentile and
throughput unit; and Grid/Focus toggles between all panels open and an accordion.

`explore.html` is the interactive drill-down for one combination at a time: an
evolution chart across runs, a payload sweep, and a per-combination ranking, with
the full control bar (kind, metric, transport, peers, payload, run, color-by).
Useful once there are many weekly runs to watch a library move over time.

`tables.html` is the numbers: for each kind and transport it renders a payload-size
by library table with the metric in each cell (msgs/s for the throughput family,
p50 with p99 for latency), best-in-row highlighted, in the style of a benchmark
report.

Each series carries the classification and library version the target reported
through `describe`, so a row reads, say, `rust-zmq 4.3.4` next to the C++
`libzmq 4.3.5`. Serve locally with `cd docs && python3 -m http.server`, since
browsers block `fetch` over `file://`.

## The weekly grid

`workflows/weekly-arena.yml` runs once a week on a self-hosted bare-metal runner
with Turbo off and C-states locked. It builds everything, runs the matrix,
renders `RANKING.md` and the dashboard archive, and commits the result. Move the
file to `.github/workflows/` to activate it once a suitable runner exists.

## Contributing

The arena provides the infrastructure; library maintainers provide the
configurations. Two kinds of pull request go through the same pipeline: a core
harness patch that touches telemetry, cgroups, or scheduling, and an
implementation tweak that adjusts socket options, batch sizes, or buffer flags.
Both land, then the next scheduled run picks them up.

Any pull request that tunes an implementation is welcome, as long as it follows
the protocol rules in `targets/README.md`: no dropped data, and a real
serialization round-trip. The harness validator enforces those, so a faster but
cheating entry fails the cell rather than the review.

## Implementation status

| piece | state |
|-------|-------|
| Cargo workspace, profiles, toolchain pins | done |
| matrix and record schema | done |
| CLI, matrix expansion, run loop | done, `--dry-run` works |
| target CLI contract and roster | done, crate versions verified |
| libzmq socket loop | all five kinds (PUSH/PULL, REQ/REP, PUB/SUB, fan-out, fan-in) over the C API |
| cgroup v2 provisioning | done (std::fs; needs root) |
| ipc and loopback tcp transport | done; netns isolation still to do |
| CPU and context-switch capture | done (`getrusage` deltas) |
| CPU and memory footprint | done; grouped across all of a cell's processes. CPU from `getrusage(RUSAGE_CHILDREN)`; memory from each process's `VmHWM` summed (unprivileged, any host), or the summed cgroup leaves when run as root |
| throughput run path | done (PUSH/PULL over ipc and tcp; drives libzmq) |
| latency run path | done (REQ/REP; target times round-trips, orchestrator parses) |
| pub/sub, fan-out, fan-in run paths | done (duration-based, multi-peer; libzmq + monocoque) |
| perf syscall counting | done (`perf_event_open` tracepoints, cgroup-scoped via `PERF_FLAG_PID_CGROUP` when run as root so every thread in the leaf is counted, including the io_threads and runtime workers that do the actual socket I/O; per-thread fallback otherwise; needs root + tracefs + `perf_event_paranoid <= 1`, else 0 with a one-time note) |
| monocoque socket loop | all five kinds (write-coalesced throughput, REQ/REP, PUB/SUB, fan-out, fan-in); run-verified locally |
| zmq.rs socket loop | throughput, latency, pub/sub (the `zeromq` 0.6 trait API); fan-out and fan-in rejected up front (engine does not multiplex multiple peers on the bound side); run-verified locally |
| rust-zmq socket loop | all five kinds via the `zmq` crate (rust-zmq) over the system libzmq; run-verified locally |
| omq socket loops | omq-tokio (epoll, current-thread + multi-thread variants) and omq-compio (io_uring) over the omq `Socket` API; throughput, latency, pub/sub, fan-in run-verified locally; fan-out runs but is gated by omq's round-robin backpressure on a shared core |
| rzmq, celerity socket loops | stubs, pending each engine's API (each already reports `describe`) |
| target classification + library version | done; every target self-reports via `describe`, the orchestrator embeds it per record, versions tracked per run |
| render and ranking generator | done and tested; emits a global ranking (mean rank across benchmarks) |
| interactive dashboard | done; filters and color-by across engine, io, threading, sync/async, native/ffi, language; per-combination and global rankings; library versions shown |

## Acknowledgments

The benchmark design is inspired by the comparison benchmark in
[omq.rs](https://github.com/paddor/omq.rs). The set of kinds (throughput,
latency, pub/sub, fan-out, fan-in), the idea of a separate bench peer per engine,
and building each implementation as its own standalone unit all come from its
`run_comparisons.py` harness. zmq-arena adds process isolation, kernel telemetry,
weekly history, and the interactive dashboard on top of that.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you state otherwise, any contribution you submit for
inclusion in this work, as defined in the Apache 2.0 license, is dual-licensed as
above, with no additional terms or conditions.
