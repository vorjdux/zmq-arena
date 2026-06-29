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

| directory | engine | crate or source | model |
|-----------|--------|-----------------|-------|
| `libzmq_cpp_target` | libzmq | system `libzmq` via CMake (C) | epoll, the C reference |
| `zeromq_rs_target` | zmq.rs | `zeromq = "0.6"` | epoll + tokio |
| `omq_tokio_target` | omq-tokio | git `paddor/omq.rs` | mio, tokio |
| `omq_compio_target` | omq-compio | git `paddor/omq.rs` | io_uring, single-thread, Linux 6.0+ |
| `rzmq_target` | rzmq | `rzmq = "0.5.21"` | io_uring + TCP_CORK, Linux |
| `celerity_target` | celerity | `celerity = "0.2.0"` | sans-IO ZMTP 3.1 + tokio |
| `monocoque_target` | monocoque | `monocoque-rs = "0.1.5"` | io_uring/compio, ZMTP 3.1 |

libzmq runs throughput, pub/sub, and latency; monocoque runs throughput. The
other Rust socket loops are stubs until each is written against its engine's
API. Crate identities and versions are verified against crates.io and the
upstream repos. See `targets/README.md` for the command-line contract and how to
add a target.

## Benchmarks and variants

The harness runs the same set of benchmarks as the omq comparison: throughput
(PUSH/PULL), latency (REQ/REP), pub/sub, fan-out, and fan-in, over ipc, loopback
tcp, and inproc, across a payload sweep, with peer counts where they apply.

A measured series is a variant, meaning an engine plus a runtime, not just an
engine. One binary can expose several runtimes, so `omq-tokio` and its
multi-thread mode, or `omq-compio` and its single-thread mode, show up as
separate series you can compare directly.

| variant | target | engine | io model | threading | selected by |
|---------|--------|--------|----------|-----------|-------------|
| `libzmq` | libzmq_cpp_target | libzmq | epoll | native threads | only variant |
| `zmq.rs` | zeromq_rs_target | zmq.rs | epoll | tokio | only variant |
| `omq_tokio` | omq_tokio_target | omq | mio | current-thread | `--variant default` |
| `omq_tokio_mt` | omq_tokio_target | omq | mio | multi-thread | `--variant multi_thread` |
| `omq_compio` | omq_compio_target | omq | io_uring | default | `--variant default` |
| `omq_compio_st` | omq_compio_target | omq | io_uring | single-thread | `--variant single_thread` |
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

`scripts/build-targets.sh` builds the orchestrator and every target, each in its
own invocation so per-target lockfiles and toolchains are honored.

## Running it

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

What runs is the throughput kind over ipc and tcp, driving libzmq as two real
processes, with CPU and context switches captured from `getrusage` and memory
from the cgroup. cgroups are skipped cleanly if you are not root.

One thing a single-vCPU VM cannot tell you is comparative performance. The two
processes share the core, cpuset pinning does nothing, and a guest cannot lock
Turbo or C-states. Treat those numbers as a wiring check. Real tail latency needs
bare metal.

## Dashboard

`docs/index.html` is a self-contained page (Apache ECharts, no build step) meant
for GitHub Pages with the source set to `docs/`. It reads the run archives under
`docs/history/` and falls back to synthetic sample data under `docs/sample/`
until the first real run lands.

It has three views driven by one control bar: an evolution chart of each variant
across weekly runs, a payload size sweep for one run, and a ranking table. You
pick the benchmark kind, metric, transport, peers, and payload; the variant
picker (with category presets) chooses which series are in play; and "color by"
groups them by engine, io model, threading, or individual variant. The metric
list follows the kind: latency quantiles for latency, msgs/s and MB/s for the
throughput family, plus CPU, context switches, syscalls, and memory throughout.

Serve it locally with `cd docs && python3 -m http.server`, since browsers block
`fetch` over `file://`.

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
| libzmq socket loop | PUSH/PULL, PUB/SUB, and REQ/REP latency over the C API |
| cgroup v2 provisioning | done (std::fs; needs root; compile-pending) |
| ipc and loopback tcp transport | done; netns isolation still to do |
| CPU and context-switch capture | done (`getrusage` + `/proc`; compile-pending) |
| throughput run path | done (PUSH/PULL over ipc and tcp; drives libzmq) |
| latency run path | done (REQ/REP; target times round-trips, orchestrator parses) |
| pub/sub, fan-out, fan-in run paths | not yet (skipped by the run loop) |
| eBPF/perf syscall counting | not yet (feature-gated) |
| monocoque socket loop | throughput (PUSH/PULL, write-coalesced) and latency (REQ/REP), TCP and IPC; run-verified locally |
| zmq.rs, omq, rzmq, celerity socket loops | stubs, pending each engine's API |
| render and ranking generator | done and tested |
| interactive dashboard | done |

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
