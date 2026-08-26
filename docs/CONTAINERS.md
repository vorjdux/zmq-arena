# Why targets are built in images and run outside them

Every target is built inside a pinned image and executed from that image's
filesystem. No target is built on the bench host, and no target runs under a
container runtime. Both halves of that are deliberate.

## Building in an image

Fifteen series across eight engines means eight toolchains, and once the arena
covers Java, .NET, Node, Python and Ruby it means several more. Installing all of
them on the machine that produces the published numbers is how a benchmark host
drifts: a library upgrade, a distribution change, a different compiler default,
and a result is no longer comparable with the one before it.

An image pins that. Each target directory has a `Dockerfile`,
`scripts/build-image.sh` builds it, and the same script flattens it with
`docker export` into `targets/<x>/rootfs/`. The image id is recorded in the run
file, so a published number identifies the exact filesystem it came from rather
than asking to be trusted.

It also means the bench host needs almost nothing: the Rust control plane, docker
to build images, and python to render. No C++ compiler, no libzmq headers, no
JDK.

## Not running in one

The measurement depends on the target being a direct child of the orchestrator:

- `getrusage(RUSAGE_CHILDREN)` supplies the cell's CPU and context switches.
- Peak RSS and the sender/receiver CPU split poll `/proc/<pid>` by child PID.
- The perf tracepoints are scoped to the cgroup leaf the harness creates.

`docker run` breaks all three. The workload ends up under containerd in a
different process tree, so the CPU total collapses to the CLI's rounding error,
the `/proc` polling follows the wrong process, and the syscall counters, scoped
to a cgroup the workload is not in, read zero. None of that fails loudly. The
numbers would still be produced; they would simply be measuring nothing, which is
the failure this project exists to avoid. Docker's bridge networking would also
add veth and NAT to every latency measurement, and `--network=host` would remove
the per-run network namespace the harness deliberately creates.

So the image is used as a filesystem, not as a runtime. The orchestrator spawns:

```
ip netns exec <ns> chroot targets/<x>/rootfs /app/target --role sub ...
```

Both wrappers `exec` rather than fork, so however many are stacked, the process
the orchestrator holds *is* the target, with the PID it was given. It sits in the
cgroup leaf the harness attached, in the network namespace the harness created,
on the host kernel. Nothing is virtualised, and there is no container runtime
anywhere in the measurement path.

`/proc` is bind-mounted into each rootfs for the life of the run, because a
chrooted process sees the host kernel but not the host's `/proc`. Native binaries
mostly do not care; managed runtimes read `/proc/self` at startup and need it.

## What it costs

Measured before adopting it, on the libzmq target, same binary and same shared
libraries, seven interleaved replicates each:

| mode | median Mmsg/s |
|---|---|
| native | 1.8442 |
| user namespace only | 1.8852 |
| user namespace + chroot | 1.8297 |

chroot came out 0.79% below native, inside a 3.1% run-to-run spread, and the
user-namespace-only case came out *faster* than native, which is physically
meaningless and confirms the three are indistinguishable at this precision. The
harness accepts a cell at 5% relative IQR, so an effect this size is below what
the methodology resolves. Mechanically that is expected: chroot changes path
resolution at `open()`, and the steady-state loop opens no files.

## One instruction set for everyone

Building once and shipping the artifact is incompatible with `-march=native`,
which pins a binary to whichever machine compiled it. That flag also applied to
only one target, so the C++ wrapper got microarchitecture-specific codegen while
the Rust targets built for generic x86-64.

Every target now compiles against the same baseline, `ARENA_ISA`, defaulting to
`x86-64-v3` (AVX2, Haswell and later). The C++ target passes it to `-march`, the
Rust targets to `-C target-cpu`. Override it for a local build that will not be
shipped, but a published run should use the default so the comparison is between
implementations rather than between build flags.
