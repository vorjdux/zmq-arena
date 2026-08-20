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

**Results live in one place: the dashboard.** A run writes one archive and the
dashboard is the only thing that reads it, so there is no second copy of the
numbers to fall out of step. See [Results](#results) for what each page answers
and how to serve it locally.

zmq-arena is not only about speed. [FEATURES.md](FEATURES.md) is the other half
of the question: which socket types and transports each implementation actually
has, whether its CURVE interoperates, whether you can use it without adopting an
async runtime. A fast library that lacks the socket type you need is not a
candidate wherever it lands on a chart.

Status: work in progress. Process isolation, cgroup pinning, all five run paths,
replication with outlier rejection, and scheduler/CPU/memory/syscall capture all
work; so do the archive, the dashboard and the registries.
[What is not done yet](#what-is-not-done-yet) lists the gaps.

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

The full roster, with the build or `--variant` flag that selects each one, is
in [targets/README.md](targets/README.md); `variants.json` is what the tooling
reads.

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
cargo run --release -p zmq-arena-orchestrator -- run --matrix matrix.linode.json --dry-run
```

A real run provisions cgroups and needs root for full isolation:

```
sudo ./target/release/zmq-arena run --matrix matrix.linode.json --run-id "$(date -u +%F)" --out scratch/
```

Each cell writes one JSON record; `scripts/render_results.py` turns a scratch
directory into the run archive the dashboard reads. On a fresh Ubuntu box,
`bash scripts/setup-ubuntu.sh` installs the toolchains first.

`matrix.linode.json` is a payload sweep over 64, 256, 1024, 4096, and 16384 byte
messages, the size set both the OMQ comparison and monocoque's own benches use,
so the arena's points line up with the numbers those projects publish. It is
generated by `scripts/gen_matrix.py`; regenerate with `make matrix`, or pass
`--sizes` for a lighter set. Count-based cells carry fewer messages at larger
payloads so they stay within budget; msgs/s and MB/s are rates, so the count
does not bias the comparison.

Telemetry is captured the same way for every cell: CPU and context switches from
`getrusage`, peak memory from the summed per-process `VmHWM`. cgroups are skipped
cleanly if you are not root; memory and CPU still record without them. Syscall
counts need perf, so they read zero unless you run under sudo (`make run-root`)
on a host with tracefs and `perf_event_paranoid <= 1`.

### Provenance is measured, not declared

The orchestrator samples the machine it is about to measure on and writes
`_host.json` next to the cell records: CPU model and count, kernel, cpufreq
governor across every online CPU, turbo/boost state, whether the CPU reports the
hypervisor flag, and whether it is running as root. The render step reads that
file. It is not told what the host was, because the render can run on a different
machine and a pasted "turbo off, C-states locked" is a claim rather than
evidence.

From those facts the run derives its own `admissible` flag: a comparison counts
only on bare metal, with the performance governor, turbo disabled, as root.
Anything else is published with the reasons attached, and every dashboard page
shows a **not admissible** badge next to the host. Nobody has to remember to
write the caveat, and nobody can leave it out.

The restrictions the run applied are recorded the same way, in `_run.json`:

| field | why it is separate from the matrix |
|---|---|
| `isolation.requested` | the cpuset and memory cap the matrix asked for |
| `isolation.applied` | whether a cgroup leaf was actually created. Without root it is not, and the cells run unpinned on the whole machine, which is a different experiment than the matrix describes |
| `replication` | the policy actually used, since `--replicates` overrides the matrix |
| `syscall_counting.captured` | whether perf registered. A host that cannot open the tracepoints records zero syscalls, which is "not measured", not "no syscalls" |

The dashboard shows all of it beside the host, and badges anything unapplied.
A run pinned to `cpuset 0-3` and one that silently ran unpinned produce numbers
that look alike and mean different things.

### Enforcing it on the bench host

Set `ZMQ_ARENA_BENCH_HOST=1` in the environment of the machine that produces
official runs. The orchestrator then treats those conditions as a gate instead
of a label and refuses to start, itemising what is wrong:

```
Error: ZMQ_ARENA_BENCH_HOST is set, so this machine is the designated benchmark
host, but it does not meet the conditions for one:
  - cpufreq governor is powersave, not performance
  - turbo/boost is enabled, so clocks are not pinned
  - not run as root, so cgroup pinning and perf counters are absent

Fix the host, or unset the variable to run it as an ordinary dev machine
(results are then recorded as not admissible).
```

The check runs before any cell, so a bench host that drifted back to `powersave`
after a reboot fails in seconds instead of spending hours producing numbers that
carry that host's authority without its guarantees. Set it in the runner's
environment rather than on a command line, so it describes the machine and not
whoever typed the command. `_host.json` records `enforced`, so an archive proves
the gate ran rather than asking a reader to assume it.

A dev machine leaves it unset and behaves exactly as before: it runs, and the
results are recorded as not admissible with the reasons attached.

A word on what a shared host can and cannot tell you. Each cell is pinned to a
4-core cpuset, so the producer and consumer no longer time-share one core and
the numbers stopped measuring core contention. That is a real improvement and
not a substitute for a bench host: a guest cannot lock Turbo or C-states, the
32-subscriber pub/sub cell necessarily oversubscribes its cpuset, and a noisy
neighbour is invisible from inside. Read a shared-host run as the payload trend
and the relative shape. Real tail latency needs bare metal. Every run records a
hardware note, and the dashboard shows it on every page.

## Results

**The dashboard is the only place results live.** A run produces one archive
under `docs/history/`, and the dashboard is the one thing that interprets it:
nothing else ranks, summarises, or re-renders those numbers. There is no second
ledger to drift out of step.

### What an archive holds

A cell record is a measurement and a reference, nothing else. Everything about
*what produced* the number lives once per run in a `builds` map:

```json
{ "schema": 2,
  "builds": {
    "monocoque@0.4.0":            { "variant": "monocoque", "engine": "monocoque", "io": "io_uring", "lib_version": "0.4.0", "...": "..." },
    "tmq@0.5.0+libzmq-4.3.4":     { "variant": "tmq", "engine": "libzmq", "binding_version": "0.5.0", "lib_version": "4.3.4", "...": "..." }
  },
  "records": [ { "build": "monocoque@0.4.0", "kind": "throughput", "payload_bytes": 64, "...": "..." } ] }
```

A build id names every version in the measured stack, so it changes the moment
any of them does. `tmq@0.5.0` alone would have stayed identical across a libzmq
4.3.4 to 4.3.5 upgrade and silently merged two different stacks under one name.

**The map is written into the archive rather than looked up in `variants.json`,
and that is the point.** An archive is history; `variants.json` is current. When
monocoque goes 0.4.0 to 0.5.0, last month's run has to keep reporting 0.4.0
forever, so it carries its own versions. `variants.json` supplies only
presentation, which is safe to keep current because a variant key never changes.

Readers accept schema 1 archives, which repeated those fields on every record,
so history published before the change still loads.

The pages are self-contained (Apache ECharts, no build step) and fall back to
synthetic sample data in `docs/sample/`, with a banner saying so, until a real
run is published.

| page | what it answers |
|---|---|
| Overview | how does every library behave across every scenario, at a glance |
| Rankings | who is ahead on one metric, as a geometric mean of each library's per-cell ratio to the libzmq baseline. One board per metric, never a blended score, because these libraries trade off differently and a single number hides it |
| Explore | one combination at a time, including how it has moved across runs |
| Tables | the raw numbers, payload size by library, best in row highlighted |
| Features | what each library supports, curated rather than measured, with unverified rows marked `declared` |

Serve it locally with `cd docs && python3 -m http.server`, since browsers block
`fetch` over `file://`.

Two curated files feed the pages and are the only place their facts live:
`features.json` (capabilities, rendered to [FEATURES.md](FEATURES.md) and the
Features page by `scripts/render_features.py`) and `variants.json` (how a series
is labelled and coloured). `scripts/render_variants.py` fails the build if the
matrix contains a variant `variants.json` does not describe, which is what used
to produce chart series labelled with a raw key.

## Publishing to GitHub Pages

The dashboard is deployed by `.github/workflows/pages.yml`. It runs on the next
push to `main` that touches `docs/`, or immediately from the Actions tab via
**Run workflow**. The workflow enables Pages itself on first run
(`configure-pages` with `enablement: true`), so **Settings > Pages** needs no
manual setup; setting Source to **GitHub Actions** by hand works too. The site
lands at
`https://<owner>.github.io/zmq-arena/`; every path in `docs/` is relative, so the
project subpath needs no configuration.

Run archives are committed under `docs/history/` like any other output, so a
deploy is a plain upload of what the repository contains. What you can see in
git is what the site serves; nothing is fetched or reconstructed.

They were gitignored at first, to keep dev-host numbers out of the repo. That
turned out to leave no path at all from a run to the dashboard: CI deploys from
a fresh checkout, so an archive that is not in git is invisible to it, and the
site could only ever show sample data. A run that should not be published should
not be rendered; every archive records the host it came from either way. The
cost is about 300 KB per run.

`pages.yml` runs on a GitHub-hosted runner and only assembles and uploads the
site, so it is safe to trigger by hand at any time; it measures nothing. Until
the first real grid run is deployed the dashboard falls back to the synthetic
data in `docs/sample/` and shows a "Sample data" banner, which is the correct
behaviour rather than an empty page.

## The weekly grid

`.github/workflows/weekly-arena.yml` runs the grid on a self-hosted bare-metal
runner with Turbo off and C-states locked. It builds everything, regenerates the
matrix, runs it, renders the run archive, commits the curated registries, and
deploys the refreshed site. It asserts the
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

## What is not done yet

Everything else in this README describes what runs today. These are the gaps:

- **Network namespaces.** The matrix names the tcp transport `tcp_netns`, but
  per-cell namespaces are not created: cells run on loopback in the host
  namespace. The name is forward-looking.
- **rzmq and celerity** are describe-only stubs. Each resolves, builds and
  reports its classification, but neither has a socket loop, so neither appears
  in the matrix.
- **No admissible run has been published.** Every archive committed so far comes
  from a dev host and says so in its hardware note. The weekly grid needs a
  bare-metal runner with Turbo off and C-states locked before any number here is
  a verdict.

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
