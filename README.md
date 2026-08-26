<p align="center">
  <img src="docs/zmq-arena-logo.svg" alt="zmq-arena: ZMTP benchmark battleground" width="520">
</p>

<p align="center">
  <b><a href="https://vorjdux.github.io/zmq-arena/">Live dashboard &rarr; vorjdux.github.io/zmq-arena</a></b><br>
  <sub>rankings, payload-size explorer, full tables and the feature matrix</sub>
</p>

# zmq-arena

A benchmarking harness for ZMTP, the ZeroMQ wire protocol. It runs several
implementations through the same isolated, instrumented conditions, so the
comparison is about the implementations and not about the harness.

Twenty series across eight families, in six languages: C++, Rust, Python,
Ruby, C# and Java.

Every runtime an engine ships is measured separately, because benchmarking a
subset would mean choosing which of an engine's configurations may represent it.
So zmq.rs appears three times, once per async runtime it supports, and the gap
between those lines is the runtime rather than the protocol code. A binding is
filed with the engine it binds, so libzmq, rust-zmq, tmq and pyzmq are one C++
engine reached four ways and the distance between them is mostly language and
wrapper overhead. Mostly, not entirely: rust-zmq vendors its own libzmq 4.3.4
rather than linking the system one, and tmq inherits that, so those two sit on a
slightly older engine than the C++ target and pyzmq. Each record carries the
engine version it ran, because a family is only a controlled comparison to the
extent the versions actually match.

- **[Results](https://vorjdux.github.io/zmq-arena/)** live only in the dashboard.
  The pages under `docs/` are what gets published; see [Results](#results) for
  what an archive holds.
- **[FEATURES.md](FEATURES.md)** covers what each library supports: socket types,
  transports, CURVE, whether it works without an async runtime.
- **[What is not done yet](#what-is-not-done-yet)** is the honest gap list.

## Quick start

Ubuntu, from a clean checkout. Targets are built inside pinned images and run
from the exported filesystem, so `setup-ubuntu.sh` installs only the control
plane's toolchain, docker and python. No C++ compiler, no libzmq headers, and
none of the target runtimes go on this machine. See
[docs/CONTAINERS.md](docs/CONTAINERS.md) for why the images are a build artifact
rather than a runtime.

```bash
bash scripts/setup-ubuntu.sh     # control plane toolchain + docker (once)
make build                       # control plane + every target image
make dry                         # expand the plan, spawn nothing
make run                         # measure, then render into docs/
make dashboard                   # serve docs/ at http://localhost:8000
```

`make run` on an ordinary machine works and is useful for checking wiring, but
its numbers are recorded as **not admissible**: without root there is no cgroup
pinning and no syscall counting, and a laptop governor is not pinned. Use
`make run-root` for the isolation the matrix asks for, and see
[Provenance](#provenance-is-measured-not-declared) for what a real comparison
requires.

A full run is 780 cells with replication; expect it to take a while. For a fast
loop, shrink the matrix and the replicate count:

```bash
python3 scripts/gen_matrix.py --sizes 64 --out matrix.quick.json
./target/release/zmq-arena run --matrix matrix.quick.json --run-id quick \
  --out scratch/quick --replicates 2
python3 scripts/render_results.py --scratch scratch/quick --run-id quick
```

## Commands

`make help` lists every target. The ones you need:

| command | does |
|---|---|
| `make build` | control plane and every runnable variant |
| `make matrix` | regenerate `matrix.linode.json` from `scripts/gen_matrix.py` |
| `make dry` | expand and print the plan without spawning anything |
| `make run` | run the matrix and render the archive into `docs/` |
| `make run-root` | the same under sudo, so cgroup pinning and perf counters apply |
| `make dashboard` | serve `docs/` over HTTP |
| `make variants` | publish `docs/variants.json` and check it covers the matrix |
| `make clean` | remove `scratch/` and all build artifacts |

The orchestrator underneath:

```
zmq-arena run [OPTIONS]
  --matrix <PATH>       matrix definition       [default: matrix.json]
  --run-id <ID>         names the archive and the cgroup paths
  --out <DIR>           per-cell JSON records   [default: scratch]
  --dry-run             expand the plan only, safe unprivileged
  --replicates <N>      fixed replicate count, overriding the matrix policy
```

And the render steps, all pure data transformation:

| script | in | out |
|---|---|---|
| `gen_matrix.py` | `--sizes`, `--cpuset` | `matrix.linode.json` |
| `render_results.py` | `--scratch` | `docs/history/<date>-run.json` |
| `render_variants.py` | `variants.json` | `docs/variants.json`, fails if the matrix has an undescribed variant |
| `render_features.py` | `features.json` | `FEATURES.md`, `docs/features.json` |

### Environment

| variable | effect |
|---|---|
| `ZMQ_ARENA_BENCH_HOST` | marks this machine the designated benchmark host: the run refuses to start unless it qualifies. See [Enforcing it](#enforcing-it-on-the-bench-host) |

## Built in a container, run outside one

Every target is built inside a pinned image and then executed **outside** any
container. Those are two separate decisions and both matter.

**Building in an image** is what keeps the machine that produces the numbers
from drifting. Thirteen implementations across seven languages means a C++
compiler, a Rust toolchain, a JDK, a .NET SDK, and Python, Ruby and Node
runtimes. Installing all of that on the bench host is how one library quietly
gets a newer compiler than it had last month. Each target directory carries a
`Dockerfile`, and `make build` builds it.

**Running outside one** is what keeps the measurement honest. The same script
then runs `docker export`, which flattens the image into a plain directory at
`targets/<name>/rootfs`, and the orchestrator launches the target with:

```
ip netns exec <ns> chroot targets/<name>/rootfs /app/target --role sub ...
```

`chroot` and `ip netns exec` both `exec` rather than fork, so the process the
orchestrator holds **is** the target, with the PID it was given. It sits in the
cgroup leaf the harness created, in the network namespace the harness created,
on the host kernel. There is no container runtime anywhere in the measurement
path, and nothing is virtualised.

That distinction is not stylistic. The telemetry depends on the target being a
direct child: CPU and context switches come from `getrusage(RUSAGE_CHILDREN)`,
peak memory and the per-end CPU split poll `/proc/<pid>`, and the syscall
counters are scoped to the harness's own cgroup. Under `docker run` the workload
lands in another process tree, so the CPU total collapses to the CLI's rounding
error, the `/proc` polling follows the wrong process, and the syscall counters
read zero. None of that fails loudly; the numbers still appear, they are just
measuring nothing. Docker's bridge networking would also put veth and NAT in
front of every latency measurement.

So the image is used as a filesystem, not as a runtime. Measured before adopting
it, same binary and same libraries: chroot cost 0.79% against a 3.1% run-to-run
spread, which is below what the harness resolves. `docs/CONTAINERS.md` has the
numbers and the reasoning.

**What the bench host needs:** docker to build the images, Rust for the control
plane, python to render, and root to run. No target toolchain, no libzmq
headers, no JVM.

Reaching the docker socket is a group membership, and a bench host is exactly
the kind of machine where it has not been granted. `setup-ubuntu.sh` adds you to
the `docker` group; until you log back in, `make build` falls back to `sudo
docker` and says so. Root here builds images, and is not the root the
measurement needs, which the orchestrator asks for separately at run time.

## What gets measured

Five patterns, over ipc and loopback tcp, across a payload sweep of 16, 64, 256,
1024, 4096 and 16384 bytes: the size set the OMQ comparison uses, so the points
line up with what that project publishes.

16 B is there deliberately. libzmq keeps messages up to roughly 33 bytes inline
in the message struct and OMQ up to roughly 55, so a sweep starting at 64 never
exercises either small-message path and the engines look more alike at the small
end than they are. Every size is a power of two, which also keeps the log axis
landing on real measured sizes.

| pattern | shape |
|---|---|
| throughput | PUSH/PULL, one producer to one consumer |
| latency | REQ/REP round trip |
| pub/sub | one publisher to 32 subscribers |
| fan-out | one producer across 4 consumers |
| fan-in | 4 producers into one consumer |

Per cell: message rate, byte rate, latency quantiles, CPU seconds, context
switches, peak RSS, and syscall counts normalised per 1000 messages. The CPU
total is also attributed to each end of the connection, so sending and receiving
cost can be compared separately. Each cell is
measured repeatedly and reported as a robust median; the replicate spread travels
with it, and a cell whose rate is beaten by a larger payload in the same sweep is
flagged as a measurement artifact rather than published as a result.

### Two tiers

Benchmarking every implementation across every pattern is neither fair nor
maintainable. The matrix splits by **pattern, never by library**:

| tier | what | who runs it |
|---|---|---|
| headline | REQ/REP, 1-to-1 PUSH/PULL, PUB/SUB at 32 subscribers, tcp | every implementation |
| extended | ipc, fan-out, fan-in | every implementation capable of it |

The only thing that excludes a library is a documented inability to serve that
pattern: zmq.rs has no fan-out or fan-in because its PUSH/PULL does not
multiplex several peers on the bound side, so it runs 30 cells per variant
where a fully capable one runs 42; celerity implements REQ/REP and PUB/SUB
only, so it runs 18. A tier whose membership were a list of favoured names
would not be a benchmark, and the ranking maths would quietly reward whichever
libraries had been let into the extra cells.

## Implementations

| directory | family | language | kind | model |
|---|---|---|---|---|
| `libzmq_cpp_target` | libzmq | C++ | implementation | epoll, the reference |
| `rust_zmq_target` | libzmq | Rust | binding | epoll, synchronous FFI |
| `tmq_target` | libzmq | Rust | binding | epoll, Tokio over rust-zmq |
| `pyzmq_target` | libzmq | Python | binding | epoll, Cython |
| `zeromq_rs_target` | zmq.rs | Rust | implementation | epoll, three runtimes |
| `omq_tokio_target` | omq | Rust | implementation | mio/epoll, three execution models |
| `pyzmq_target` | omq | Python | binding | the same wrapper, `--variant pyomq` |
| `omq_rb_target` | omq | Ruby | implementation | fibres, pure Ruby |
| `monocoque_target` | monocoque | Rust | implementation | io_uring or epoll, three runtimes |
| `rzmq_target` | rzmq | Rust | implementation | epoll or io_uring |
| `celerity_target` | celerity | Rust | implementation | tokio; REQ/REP and PUB/SUB only |
| `netmq_target` | netmq | C# | implementation | epoll, a port of libzmq |
| `jeromq_target` | jeromq | Java | implementation | epoll, a port of libzmq |

An **implementation** speaks ZMTP itself. A **binding** calls into someone
else's engine, so its row answers what a language costs rather than how good an
implementation it is: pyzmq is libzmq, and the distance between them is CPython.
The dashboard says which is which, because reading a binding as an
implementation gets the conclusion backwards.

Two families are worth knowing about. The libzmq family holds one engine reached
four ways. The omq family holds a Rust implementation, its Python binding, and a
separate pure-Ruby implementation of the same project, so `family` and `engine`
are recorded separately rather than one standing in for the other.

A measured series is a **variant**: an engine plus a runtime. Every runtime an
engine ships gets one, because benchmarking a subset would mean choosing which
of an engine's configurations may represent it. The roster, with the build or
`--variant` flag that selects each, is in
[targets/README.md](targets/README.md); `variants.json` is what the tooling
reads.

### Why each target is its own project

Each target under `targets/` owns its `Cargo.toml`, `Cargo.lock`, release
profile and toolchain pin, and is deliberately not a workspace member. A shared
workspace resolves one dependency graph and one toolchain across every member,
so each implementation would be measured against whatever the resolver settled
on rather than what it ships. Standalone builds let `zeromq` pin its own tokio,
`netmq` publish self-contained against its own .NET runtime, and a future Go
target use its native toolchain. Each is built inside its own image and run from the exported
filesystem, which is also what keeps those toolchains off the machine that
produces the numbers.

Adding a target means implementing the command-line contract in
[targets/README.md](targets/README.md) and adding a `Dockerfile` beside it; it
can be written in any language, and its toolchain never touches the bench host.

## Isolation and telemetry

Every engine gets the same **IO-lane budget**, one background IO thread, set by
the matrix rather than inherited from whatever each runtime defaults to. This
matters most where it is least visible: a tokio runtime sizes its worker pool to
the machine while libzmq defaults to a single IO thread, so a 32-subscriber
PUB/SUB cell would otherwise compare one engine's single lane against another's
four. omq's `multi_thread` variant is the deliberate exception, because running
on more than one lane is what that variant is.

Each cell runs in a cgroup v2 leaf with the cpuset and memory cap the matrix
declares, so the producer and consumer do not time-share a core. Which CPUs those
are is a per-machine decision: the default `0-3` is right only when they are
four distinct physical performance cores. On a part that enumerates SMT siblings
adjacently, `0-3` is two physical cores running both threads of each, and the two
ends of a cell contend for the same execution units; on Intel hybrid parts the
high-numbered CPUs are efficiency cores clocked differently. Read the topology
with `lscpu -e=CPU,CORE,MAXMHZ` and regenerate with
`gen_matrix.py --cpuset 0,2,4,6` or whatever that host's performance cores are. Cells on the
`tcp_netns` transport run inside a network namespace created for the run, so
loopback carries their traffic and nothing else; both peers of a cell share the
namespace, which is why it is per run rather than per process. CPU and context
switches come from `getrusage`, peak memory from the summed per-process `VmHWM`,
and syscall counts from `perf_event_open` tracepoints scoped to the cgroup.

Each syscall counter names a **role** in the data path rather than one syscall,
because which syscall fills that role is the runtime's choice: the send counter
traces `sendmsg`, `sendto`, `sendmmsg` and `writev`, and the wait counter traces
`epoll_wait`, `epoll_pwait`, `epoll_pwait2`, `poll` and `ppoll`. Counting a
single spelling measured taste rather than behaviour, and produced the worst
kind of wrong number: every implementation here sends with `sendto`, so an
earlier build that traced only `sendmsg` recorded zero for all of them, and the
async-io runtimes, which also wait in `epoll_pwait`, recorded zero for every
counter the efficiency board adds up. A zero there reads as "no kernel work",
which is the best possible score, so the least understood runtime won.

A cell that moved messages had to hand bytes to the kernel somehow, so a
data-path total of zero is now reported as unmeasured rather than as zero, and
the dashboard leaves a gap instead of a value. `read` and `write` are
deliberately not traced: they are not socket-specific, and libzmq makes about
13k of them per cell on its internal signaller, which would count as payload.

`getrusage` gives an exact CPU total for the cell but cannot say which process
spent it, so the sender/receiver split comes from sampling `/proc/<pid>/stat` on
the same 20 ms poll that tracks peak RSS. Those samples can miss the last tick
before a process exits, so they are not published as absolute values: only their
ratio is used, applied to the exact total. The two halves therefore always sum to
the cell's CPU seconds.

cgroups, network namespaces and perf all need root. Without it the run still works and still records
CPU and memory, but the cells are unpinned and the syscall counters read zero.
The archive records which of those happened, so the difference is visible rather
than assumed.

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
| `netns.applied` | whether the run got its own network namespace. `tcp_netns` cells run on a private loopback with nothing else on it; without root they fall back to host loopback, shared with every other process on the machine |
| `replication` | the policy actually used, since `--replicates` overrides the matrix |
| `syscall_counting.captured` | whether perf registered *and* the data path was actually seen. A host that cannot open the tracepoints records zero syscalls, and so does a runtime whose syscalls this build does not trace; both are "not measured", not "no syscalls" |

The dashboard shows all of it beside the host, and badges anything unapplied.
A run pinned to `cpuset 0,2,4,6` and one that silently ran unpinned produce numbers
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

**The dashboard is the only place results live**, published at
<https://vorjdux.github.io/zmq-arena/>. A run produces one archive
under `docs/history/`, and the dashboard is the one thing that interprets it:
nothing else ranks, summarises, or re-renders those numbers. There is no second
ledger to drift out of step.

### What an archive holds

A cell record is a measurement and a reference, nothing else. Everything about
*what produced* the number lives once per run in a `builds` map:

```json
{ "schema": 2,
  "builds": {
    "jeromq@0.6.0":               { "variant": "jeromq", "engine": "jeromq", "io": "epoll", "lib_version": "0.6.0", "...": "..." },
    "tmq@0.5.0+libzmq-4.3.4":     { "variant": "tmq", "engine": "libzmq", "binding_version": "0.5.0", "lib_version": "4.3.4", "...": "..." }
  },
  "records": [ { "build": "jeromq@0.6.0", "kind": "throughput", "payload_bytes": 64, "...": "..." } ] }
```

A build id names every version in the measured stack, so it changes the moment
any of them does. `tmq@0.5.0` alone would have stayed identical across a libzmq
4.3.4 to 4.3.5 upgrade and silently merged two different stacks under one name.

**The map is written into the archive rather than looked up in `variants.json`,
and that is the point.** An archive is history; `variants.json` is current. When
pyzmq goes 27.2.0 to 28.0.0, last month's run has to keep reporting 27.2.0
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

A run that should not be published should not be rendered; every archive records
the host it came from either way. About 300 KB per run.

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

- **inproc.** The transport is in the schema but no target implements it, so the
  matrix does not schedule it. It measures an in-process queue rather than a
  wire protocol, which is a different question from the one this harness asks.
- **A second machine.** Every published run comes from one host. Comparing the
  same libraries across CPU vendors and kernels would say more than another
  digit of precision on this one.

## Running a measured cell without Docker

Docker is a build-time dependency. Once `make build` has run, the daemon can be
stopped and every measurement still works, because what the orchestrator needs
is a directory, not a container.

An exported target is a plain filesystem. This runs one, by hand, with docker
shut down:

```bash
sudo systemctl stop docker            # prove it is not involved
sudo mount --bind /proc targets/pyzmq_target/rootfs/proc
sudo chroot targets/pyzmq_target/rootfs /app/target describe
```

That prints the target's classification, read out of the image's own Python and
its own libzmq, with neither installed on the host. The orchestrator does
exactly this, adding the cgroup, the network namespace, and the arguments for
the cell.

It also means the images do not have to be built where they are measured. Build
them on a workstation or in CI, copy `targets/*/rootfs` to the bench host, and
that host never needs docker at all. The run record carries each image's id, so
a published number identifies the filesystem it came from rather than asking to
be trusted.

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
