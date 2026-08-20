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

zmq-arena is not only about speed. [FEATURES.md](FEATURES.md) is the other half
of the question: which socket types and transports each implementation actually
has, whether its CURVE interoperates, whether you can use it without adopting an
async runtime. A fast library that lacks the socket type you need is not a
candidate wherever it lands on a chart.

Status: work in progress. The data and reporting side is real and tested: run
archives, rankings, and the dashboard all work. The measurement
side is mostly real: process isolation, cgroup pinning, all five run paths,
replication with outlier rejection, and scheduler/CPU/memory/syscall capture
work. Per-cell network namespaces are not wired (the `tcp_netns` transport runs
on host loopback today), and two engine wrappers are still stubs. See the status
table near the end.

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
| `tmq_target` | libzmq | Rust | `tmq = "0.5"` | epoll, Tokio bindings over rust-zmq |
| `zeromq_rs_target` | zmq.rs | Rust | `zeromq = "0.6"` | epoll + tokio |
| `omq_tokio_target` | omq-tokio | Rust | `omq-tokio = "0.21.3"` | mio, tokio |
| `rzmq_target` | rzmq | Rust | `rzmq = "0.5.25"` | io_uring + TCP_CORK, Linux |
| `celerity_target` | celerity | Rust | `celerity = "0.1.1"` | sans-IO ZMTP 3.1 + tokio |
| `monocoque_target` | monocoque | Rust | `monocoque-rs = "0.4.0"` | io_uring/compio or tokio, ZMTP 3.1 |

Three targets reach the same engine, libzmq, by different routes: `libzmq` is
the C++ peer, `rust_zmq` is the synchronous Rust binding, and `tmq` wraps that
binding in futures for Tokio. The gaps between the three are binding overhead
and async-wrapper overhead, isolated from any protocol difference, which is a
cleaner measurement than comparing two engines that differ in everything at
once. monocoque picks its runtime at compile time, so
its wrapper builds twice and reports two variants (compio/io_uring and
tokio/epoll); omq-tokio likewise exposes current-thread and multi-thread runtimes
as two variants. rzmq and celerity remain stubs until each is written against its
engine's API. Crate identities and versions are verified against crates.io and
the upstream repos. See `targets/README.md` for the command-line contract and how
to add a target, and [FEATURES.md](FEATURES.md) for what each implementation
supports beyond speed.

**omq-compio is gone.** The io_uring backend no longer exists upstream: the
omq.rs workspace is `omq-tokio` and `omq-libzmq`, and the target's git dependency
stopped resolving. The wrapper has been removed rather than left to rot.

Which implementations run which benchmarks is a matter of policy, not of what the
harness can express -- see [Two tiers](#two-tiers) below.

## Two tiers

Benchmarking every implementation across every pattern is neither fair nor
maintainable. An engine that never set out to multiplex many peers on a bound
socket should not be scored on fan-out, and each added implementation multiplies
the grid. The arena follows the policy the OMQ maintainer settled on for the same
reason, and splits the matrix in two:

| tier | what | why |
|---|---|---|
| **headline** | REQ/REP latency, 1-to-1 PUSH/PULL throughput, PUB/SUB at 32 subscribers, tcp only, swept over the payload sizes | the three patterns every implementation here genuinely implements, so a comparison is a comparison |
| **extended** | ipc, fan-out, fan-in | the rest of the coverage, for everything able to run it |

**The tiers split by pattern, never by library.** Every implementation runs every
cell in both tiers it is capable of running, and the only thing that excludes one
is a documented inability to serve that pattern: zmq.rs has no fan-out or fan-in
because its PUSH/PULL does not multiplex several peers on the bound side, so it
runs 25 cells per variant where everything else runs 35. A tier whose membership
were a list of favoured names would not be a benchmark, and the ranking maths
would quietly reward whichever libraries had been let into the extra cells.

Twelve variants at 35 cells (25 for the three zmq.rs runtimes) is 390 cells.

## Benchmarks and variants

The harness runs the same set of benchmarks as the omq comparison: throughput
(PUSH/PULL), latency (REQ/REP), pub/sub, fan-out, and fan-in, over ipc, loopback
tcp, and inproc, across a payload sweep, with peer counts where they apply.

A measured series is a variant, meaning an engine plus a runtime, not just an
engine. **Every runtime an engine ships gets its own variant**, with no
exceptions: benchmarking only some of them would mean picking which of an
engine's configurations is allowed to represent it, and that choice would decide
results before any measurement happened. So monocoque appears three times
(compio/io_uring, tokio, smol), zmq.rs three times (tokio, async-std,
async-dispatcher), and omq three times (tokio current-thread, tokio
multi-thread, and its synchronous blocking API over library-owned IO threads).
tmq and rust-zmq ship one runtime each. Twelve series in total.

Comparing an engine against itself across runtimes is often more informative than
comparing two engines: the protocol code is identical, so the difference is the
executor and the IO model. omq's `blocking` variant is worth singling out, since
it is a sync API over library-owned IO threads, exactly libzmq's model, which
makes that pair a direct comparison of two implementations of the same idea.

| variant | target | engine | io model | threading | selected by |
|---------|--------|--------|----------|-----------|-------------|
| `libzmq` | libzmq_cpp_target | libzmq | epoll | native threads | only variant |
| `rust_zmq` | rust_zmq_target | libzmq | epoll | native threads | only variant |
| `tmq` | tmq_target | libzmq | epoll | native threads + tokio | only variant |
| `zeromq_rs` | zeromq_rs_target | zmq.rs | epoll | tokio | default build |
| `zeromq_rs_async_std` | zeromq_rs_target | zmq.rs | epoll | async-std | `--features async-std-rt` |
| `zeromq_rs_async_dispatcher` | zeromq_rs_target | zmq.rs | epoll | async-dispatcher | `--features async-dispatcher-rt` |
| `omq_tokio` | omq_tokio_target | omq | mio/epoll | current-thread | `--variant default` |
| `omq_tokio_mt` | omq_tokio_target | omq | mio/epoll | multi-thread | `--variant multi_thread` |
| `omq_blocking` | omq_tokio_target | omq | mio/epoll | sync API, omq-owned IO threads | `--variant blocking` |
| `rzmq` | rzmq_target | rzmq | io_uring | tokio | only variant |
| `celerity` | celerity_target | celerity | epoll | tokio | only variant |
| `monocoque` | monocoque_target | monocoque | io_uring | compio | default build |
| `monocoque_tokio` | monocoque_target | monocoque | epoll | tokio | `--no-default-features --features tokio` |
| `monocoque_smol` | monocoque_target | monocoque | epoll | smol | `--no-default-features --features smol` |

Each record carries the variant's category tags (engine, io model, threading),
which is what lets the dashboard group and compare by category.

## Build

The orchestrator is a small workspace; build it on its own:

```
cargo build --release -p zmq-arena-orchestrator
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

`scripts/build-targets.sh` builds the control plane and every target, each in its
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
messages (the size set both the OMQ comparison and monocoque's own benches
sweep, so the arena's points line up with the numbers those projects publish)
across all five kinds and
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

A word on what a shared host can and cannot tell you. The matrix now pins each
cell to a 4-core cpuset, so the producer and consumer no longer time-share one
core the way they did on a single-vCPU box, and the numbers stopped measuring
core contention. That is a real improvement and not a substitute for a bench
host: a guest still cannot lock Turbo or C-states, the 32-subscriber pub/sub cell
necessarily oversubscribes its cpuset, and a noisy neighbour is invisible from
inside. Read a shared-host run as the payload trend and the relative shape. Real
tail latency needs bare metal, and `RANKING.md` says so on any run whose hardware
note marks it a dev-host test.

## Dashboard

The `docs/` pages are self-contained (Apache ECharts, no build step) and meant
for GitHub Pages with the source set to `docs/`. They read the run archives under
`docs/history/` and fall back to synthetic sample data under `docs/sample/` until
the first real run lands. A top nav links five pages.

`index.html` is the Overview: the landing page. It is a grid of small-multiple
panels, one per scenario (throughput ipc, latency tcp, pub/sub, fan-out, fan-in,
...), each plotting payload size against the metric with one line per library.
Every library keeps the same colour in every panel, so you can read the whole
comparison at a glance without picking. The library chips act as a shared legend
and filter; a segmented control switches the latency percentile and throughput
unit; and Grid/Focus toggles between all panels open and an accordion.

`rankings.html` is the per-metric leaderboards, one board per metric rather than
one blended score, because these libraries trade off differently and a single
number hides it. Each board is the geometric mean of a library's per-cell ratio
to the libzmq baseline: magnitude-aware, dimensionless, higher is better, with
inverted cells dropped and gaps inside the replicate noise counted as ties.
Performance boards cover latency and each message-rate workload separately;
efficiency boards cover CPU, context switches, syscalls and memory per message.

`explore.html` is the interactive drill-down for one combination at a time: an
evolution chart across runs, a payload sweep, and a per-combination ranking, with
the full control bar (kind, metric, transport, peers, payload, run, color-by).
Useful once there are many weekly runs to watch a library move over time.

`tables.html` is the numbers: for each kind and transport it renders a payload-size
by library table with the metric in each cell (msgs/s for the throughput family,
p50 with p99 for latency), best-in-row highlighted, in the style of a benchmark
report.

`features.html` is the feature matrix, and it is the one page here that does not
come from a run: socket types, transports, platforms, bindings, whether CURVE
works, and whether the library is usable without adopting an async runtime. It is
curated from each project's documentation and marks every unverified row
`declared` rather than passing a claim off as a measurement. Source of truth is
`features.json`; `scripts/render_features.py` regenerates it and
[FEATURES.md](FEATURES.md).

How a series is labelled and coloured comes from `variants.json`, which every
surface reads: the dashboard pages fetch it and the result renderer takes its
category tags from it. That is
one file to edit when a variant is added, and `scripts/render_variants.py`
(`make variants`) fails the build if the matrix contains a variant it does not
describe, which is what used to produce chart series labelled with a raw key.

Serve the dashboard locally with `cd docs && python3 -m http.server`, since
browsers block `fetch` over `file://`.

## Publishing to GitHub Pages

The dashboard is deployed by `.github/workflows/pages.yml`. It runs on the next
push to `main` that touches `docs/`, or immediately from the Actions tab via
**Run workflow**. The workflow enables Pages itself on first run
(`configure-pages` with `enablement: true`), so **Settings > Pages** needs no
manual setup; setting Source to **GitHub Actions** by hand works too. The site
lands at
`https://<owner>.github.io/zmq-arena/`; every path in `docs/` is relative, so the
project subpath needs no configuration.

Run archives are the one subtlety. `docs/history/*` is gitignored so dev-host
runs never churn the repo, which means a CI checkout starts with no archives at
all and a naive deploy would publish a site containing only the newest run,
flattening the Explore page's evolution chart. So the published site is itself
the store: `scripts/sync_history.py` downloads the archives already live and
restores them before the upload, then rebuilds `history/index.json` from what is
actually on disk. `--keep` bounds it (26 runs by default, about six months of
weekly runs), so the deployment stays a fixed size however long the grid runs.
The weekly job also uploads each archive as a 90-day CI artifact, so a botched
deploy cannot take a run's raw data with it.

The split of duties is deliberate:

| what | where it lives |
|---|---|
| `RANKING.md`, `FEATURES.md`, `docs/features.json`, `docs/variants.json` | committed to `main`, overwritten in place each run, so the file count is fixed |
| `docs/history/*-run.json` | never committed; carried forward by the Pages deployment |

That keeps `main` code-only and stops it growing without bound as
implementations release and get re-benchmarked.

`pages.yml` runs on a GitHub-hosted runner and only assembles and uploads the
site, so it is safe to trigger by hand at any time; it measures nothing. Until
the first real grid run is deployed the dashboard falls back to the synthetic
data in `docs/sample/` and shows a "Sample data" banner, which is the correct
behaviour rather than an empty page.

## The weekly grid

`.github/workflows/weekly-arena.yml` runs the grid on a self-hosted bare-metal
runner with Turbo off and C-states locked. It builds everything, regenerates the
matrix, runs it, renders `RANKING.md` and the run archive, commits the repo-facing documents, and deploys the refreshed site. It asserts the
runner is performance-locked before measuring anything.

**Its weekly `schedule:` is commented out on purpose.** The job needs a runner
labelled `[self-hosted, bare-metal, perf-locked]`, and until one is registered a
live cron would queue a job every Monday that nothing can pick up. Uncomment the
block once the runner exists; `workflow_dispatch` is enabled meanwhile, so the
grid can be triggered by hand from the Actions tab.

Deployment is a separate `publish` job so the multi-hour measuring job does not
hold the Pages concurrency lock, and it shares that lock with `pages.yml` so the
two can never deploy on top of each other.

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
| ipc and loopback tcp transport | done. The matrix names the tcp transport `tcp_netns`, but per-cell network namespaces are NOT yet created: cells run on loopback in the host namespace. The name is forward-looking and the isolation is still to do |
| CPU and context-switch capture | done (`getrusage` deltas) |
| CPU and memory footprint | done; grouped across all of a cell's processes. CPU from `getrusage(RUSAGE_CHILDREN)`; memory from each process's `VmHWM` summed (unprivileged, any host), or the summed cgroup leaves when run as root |
| throughput run path | done (PUSH/PULL over ipc and tcp; drives every target) |
| latency run path | done (REQ/REP; target times round-trips, orchestrator parses) |
| pub/sub, fan-out, fan-in run paths | done (duration-based, multi-peer). Every target implements them except zmq.rs, whose PUSH/PULL cannot multiplex several peers on the bound side, so it runs pub/sub but not the fan patterns |
| perf syscall counting | done (`perf_event_open` tracepoints, cgroup-scoped via `PERF_FLAG_PID_CGROUP` when run as root so every thread in the leaf is counted, including the io_threads and runtime workers that do the actual socket I/O; per-thread fallback otherwise; needs root + tracefs + `perf_event_paranoid <= 1`, else 0 with a one-time note) |
| monocoque socket loop | all five kinds on monocoque-rs 0.4.0, both runtimes (compio io_uring + tokio epoll); tuned to match the engine's own bench peer (full-slab reads, coalesced writes, `send_one` / `recv_into` so the measured loops allocate nothing per message); run-verified locally |
| zmq.rs socket loop | throughput, latency, pub/sub (the `zeromq` 0.6 trait API); fan-out and fan-in rejected up front (engine does not multiplex multiple peers on the bound side); run-verified locally |
| rust-zmq socket loop | all five kinds via the `zmq` crate (rust-zmq) over the system libzmq; run-verified locally |
| omq socket loops | omq-tokio 0.21.3 from crates.io (epoll, current-thread + multi-thread variants) over the omq `Socket` API; options match the engine's own `bench_options()`; headline tier only. omq-compio removed: the io_uring backend no longer exists upstream |
| rzmq, celerity socket loops | stubs, pending each engine's API (each already reports `describe`) |
| target classification + library version | done; every target self-reports via `describe`, the orchestrator embeds it per record, versions tracked per run |
| render and ranking generator | done and tested; emits per-metric boards scored as the geometric mean of each library's ratio to the libzmq baseline, and stamps the run's host and admissibility onto `RANKING.md` |
| interactive dashboard | done; five pages (Overview, Rankings, Explore, Tables, Features); filters and color-by across engine, io, threading, sync/async, native/ffi, language; library versions shown |
| feature matrix | done; curated in `features.json`, rendered to `FEATURES.md` and `docs/features.html`, with unverified rows marked `declared` |
| matrix tiering | done; split by pattern, not by library. headline (tcp, the three universal patterns) + extended (ipc, fan-out, fan-in), each run by every implementation capable of it |

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
