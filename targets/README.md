# Target wrapper contract

Every target is a standalone binary spawned by the orchestrator as a distinct OS
process. The harness owns isolation and measurement; the wrapper owns the socket
implementation and its tuning. This file is the contract a wrapper must honor to
participate in the grid.

## Unified CLI

All wrappers parse the same flags. The orchestrator passes them verbatim.

```
--role <pub|sub>          producer or consumer end (see the kinds table for the
                          concrete socket each role maps to per kind)
--kind <throughput|latency|pubsub|fanout|fanin>
--transport <tcp|ipc|inproc>   tcp = loopback inside a netns; ipc = unix socket;
                          inproc = in-process (single process, no isolated pair)
--endpoint <string>       full ZMTP endpoint, e.g. tcp://127.0.0.1:5555 or ipc:///run/arena.sock
--payload-bytes <u32>     fixed message size
--messages <u64>          steady-state measurement count
--warmup <u64>            messages discarded before measurement
--peers <u32>             subscriber/pusher count for pubsub/fanout/fanin
--variant <name>          runtime variant selector (see Variants below)
--knob <key=value>        repeatable; maintainer-owned tuning (see below)
```

## describe: self-reported classification

A wrapper invoked as `<binary> describe` (no other flags) prints one line of JSON
to stdout and exits 0. The orchestrator runs this once per binary and embeds the
result in every record, so the dashboard can filter and rank by classification and
track library-version evolution across runs without a central registry. The
target is the source of truth.

```json
{"engine":"libzmq","lib_version":"4.3.5","binding_version":null,
 "lib_language":"C++","impl":"native","ffi_to":null,"language":"Rust",
 "concurrency":"sync","threading":"native","io":"epoll"}
```

| field | meaning |
|-------|---------|
| `engine` | the implementation being measured (libzmq, zmq.rs, omq, rzmq, celerity, monocoque) |
| `lib_version` | the engine's own version. Read it from the linked library where possible (libzmq: `zmq_version()`); a build script that parses Cargo.lock supplies it for pure-Rust crates, so it tracks the lockfile |
| `binding_version` | for an FFI binding, the binding crate's version; `null` when native |
| `lib_language` | the engine's implementation language (libzmq is C++; the pure-Rust engines are Rust) |
| `impl` | `native` if the wrapper language reaches the engine directly, `ffi` if through a foreign binding |
| `ffi_to` | the language the FFI calls into (`C` for the libzmq binding); `null` when native |
| `language` | the wrapper / what you write socket code against (C++ for the libzmq C++ target, Rust for the rest) |
| `concurrency` | `sync` or `async` |
| `threading` | `single`, `multi`, or `native` (OS threads) |
| `io` | readiness or completion model: `epoll`, `io_uring`, ... |

A target with a runtime-selected `--variant` whose threading or concurrency differs
per variant should make `describe` variant-aware (read `--variant` and branch); the
current targets are single-variant, so `describe` is binary-level. Implement it as
a fast path before argument parsing. Pure-Rust targets get `lib_version` from a
`build.rs` that reads the engine crate's entry in the committed Cargo.lock.

## Benchmark kinds

The harness drives five kinds, mirroring the omq comparison matrix. `--kind`
selects the kind; `--role` plus `--peers` select this process's part.

| kind | producer role (`--role pub`) | consumer role (`--role sub`) | peers | metric |
|------|------------------------------|------------------------------|-------|--------|
| `throughput` | PUSH, sends | PULL, counts | none | msgs/s, MB/s |
| `latency` | REQ client, round-trips | REP server | none | p50/p90/p99/p99.9 |
| `pubsub` | PUB, sends | SUB, counts | N subscribers | msgs/s, MB/s |
| `fanout` | one PUSH | one of N PULL | N pulls | msgs/s (x peers) |
| `fanin` | one of N PUSH (connect) | one PULL (bind) | N pushes | msgs/s |

`fanout` and `fanin` are TCP-only upstream. `inproc` applies to `throughput`
and `latency` only and runs in a single process, so it is the one kind/transport
combination that does not honor the arena's process-isolation rule; it is
included for parity with omq and flagged as such in the records.

## Variants

A single target binary may expose more than one runtime. Each runtime is a
distinct measured series (a *variant*), selected with `--variant`. This mirrors
omq, where `omq-tokio` / `omq-tokio-mt` and `omq-compio` / `omq-compio-st` are
the same binary in different runtime modes.

| variant | binary / target dir | engine | io model | threading | how selected |
|---------|---------------------|--------|----------|-----------|--------------|
| `libzmq` | libzmq_cpp_target | libzmq | epoll | native threads | single variant |
| `zmq.rs` | zeromq_rs_target | zmq.rs | epoll | tokio | single variant |
| `omq_tokio` | omq_tokio_target | omq | mio/epoll | current-thread | `--variant default` |
| `omq_tokio_mt` | omq_tokio_target | omq | mio/epoll | multi-thread | `--variant multi_thread` |
| `omq_compio` | omq_compio_target | omq | io_uring | default | `--variant default` |
| `omq_compio_st` | omq_compio_target | omq | io_uring | single-thread | `--variant single_thread` |
| `rzmq` | rzmq_target | rzmq | io_uring | tokio | single variant |
| `celerity` | celerity_target | celerity | epoll | tokio | single variant |
| `monocoque` | monocoque_target | monocoque | io_uring | thread-per-core | placeholder |

The wrapper maps `--variant` to its engine's runtime configuration (for omq-tokio
this is the tokio runtime flavor; for omq-compio the compio runtime). Each
variant carries category tags (engine, io model, threading) in the result
record, so the dashboard can compare any subset of variants or group them by
category. Variants in the same category (for example all `io_uring` engines, or
all `single`-thread runtimes) are directly comparable.

## Knob convention

Tuning belongs to the implementation, not the harness. The harness never sets a
socket option itself; it forwards whatever `--knob key=value` pairs the matrix
declared. A wrapper applies the knobs it understands and ignores keys it does
not, so the matrix can carry one superset of knobs across every implementation.

Recommended keys where the underlying library supports them:

| key          | meaning                              |
|--------------|--------------------------------------|
| `sndhwm`     | send high-water mark (messages)      |
| `rcvhwm`     | receive high-water mark (messages)   |
| `io_threads` | ZMQ context I/O thread count         |
| `batch_size` | application-level send batch         |
| `sq_depth`   | io_uring submission queue depth      |
| `tcp_nodelay`| disable Nagle (where exposed)        |

Environment-variable form is also accepted: `ARENA_KNOB_SNDHWM=1000`. CLI flags
win over environment on conflict.

## Protocol rules

A submission is valid only if it respects these. PRs that violate them are
rejected by the harness validator, not by maintainer review.

1. No data dropping. The subscriber must receive exactly `messages` payloads in
   the measurement block. HWM-induced drops on a `pub_sub` socket count as a
   failed cell.
2. True serialization bounds. The payload must cross the socket and be read back
   on the consumer; no eliding the round-trip.
3. Single measurement block. Warmup is separate; steady-state count is fixed by
   the matrix, not the wrapper.

## Endpoint and IPC notes

The orchestrator passes a full endpoint via `--endpoint`. IPC addressing is not
uniform across engines: zmq.rs and rzmq use pathname sockets (`ipc:///tmp/...`),
while libzmq and omq also accept Linux abstract-namespace sockets (`ipc://@name`).
A matrix cell must use the form its target supports. celerity fails closed on
non-loopback TCP unless CURVE-RS is configured, so netns-loopback cells must use
a loopback address.

## Adding a target

1. Create `targets/<name>_target/` as a standalone project. Do not add it to the
   orchestrator's `Cargo.toml` workspace. Each target owns its dependency
   closure, `[profile.release]`, `Cargo.lock`, and toolchain pin so it is
   measured exactly as it ships. A shared workspace would unify versions,
   features, and the release profile across implementations and skew the
   comparison.
2. Implement the unified CLI and the two roles.
3. Add a build command to `scripts/build-targets.sh` (a per-directory `cargo
   build --release` for Rust, or the native build for another toolchain). The
   contract with the harness is the spawned-process CLI, not a shared crate, so
   any language is admissible.
4. Reference the compiled binary path from the matrix entry.
