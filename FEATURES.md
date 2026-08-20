# Feature matrix

What each implementation supports, next to what it measures. A fast library that lacks the socket type you need, or whose CURVE does not interoperate, is not a candidate no matter where it lands on a chart.

> **This page is curated, not measured.** Every other number in this repo comes out of a run. These rows come from each project's own documentation, read on 2026-08-20. `yes` means the project documents it *and* zmq-arena exercises it; **`declared` means we are repeating the project's claim and have not tested it**. Two of the implementations below are describe-only stubs, so every capability they list is unverified.

Regenerate with `python3 scripts/render_features.py` after editing `features.json`.

## Matrix

| capability | libzmq | rust-zmq | tmq | zmq.rs | omq | monocoque | rzmq | celerity |
|---|---|---|---|---|---|---|---|---|
| version | 4.3.5 | 0.10 (libzmq 4.3.4) | 0.5.0 (libzmq 4.3.4) | 0.6.0 | 0.21.3 | 0.4.0 | 0.5.25 | 0.1.1 |
| language | C++ | Rust | Rust | Rust | Rust | Rust | Rust | Rust |
| implementation | native | FFI to libzmq | FFI to libzmq | native | native | native | native | native |
| socket types | 12 | 12 | 12 | 9 | 11 | 12 | declared | declared |
| transports | tcp, ipc, inproc, udp, pgm, epgm, tipc, vmci | tcp, ipc, inproc, udp, pgm, epgm | tcp, ipc, inproc, udp, pgm, epgm | tcp, ipc | tcp, ipc, inproc, udp, ws, wss, lz4+tcp, zstd+tcp | tcp, ipc | tcp, ipc | tcp |
| NULL | yes | yes | yes | yes | yes | yes | declared | declared |
| PLAIN | declared | declared | declared | no | declared | declared | declared | declared |
| CURVE | declared | declared | declared | no | declared | declared | declared | declared |
| usable without an async runtime | yes | yes | no | no | yes | no | no | partial |
| platforms | Linux, macOS, Windows, BSD | Linux, macOS, Windows | Linux, macOS, Windows | Linux, macOS, Windows (tcp only; ipc is unix-only) | Linux, macOS, Windows | Linux (io_uring, 5.6+ for the compio backend), portable via the tokio/smol backends | Linux | Linux, macOS, Windows |
| bindings | Reference implementation; bindings exist for most languages. | Is itself the Rust binding to libzmq. | Is itself an async Rust binding, layered on rust-zmq. | None. | Go, Java, Lua, Node, Python (pyomq). | None. | None. | None. |
| benchmarked here | headline + extended (reference) | headline | headline + extended | headline | headline | headline + extended | not benchmarked (target is a describe-only stub) | not benchmarked (target is a describe-only stub) |

## Notes

### libzmq 4.3.5

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Synchronous API over library-owned background IO threads; no async runtime in the caller.
- Source: zeromq/libzmq README and zmq_socket(3)

### rust-zmq 0.10 (libzmq 4.3.4)

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Inherits libzmq's synchronous API and IO threads; it is a binding, so capability follows the linked libzmq.
- Source: erickt/rust-zmq README; capability is the linked libzmq's

### tmq 0.5.0 (libzmq 4.3.4)

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Requires Tokio: its sockets are futures Sinks and Streams. The libzmq underneath still runs its own IO threads, so the Tokio runtime drives only the wrapper.
- Not an engine: an async facade over rust-zmq, which binds libzmq. Capability therefore follows the linked libzmq, and the series exists to isolate binding and async-wrapper overhead against the libzmq and rust-zmq targets. Socket construction matches the tmq peer in the omq.rs comparison harness, which sets no socket options.
- Source: cetra3/tmq README and crates.io; capability is the linked libzmq's

### zmq.rs 0.6.0

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, PUSH, PULL
- Runtime: Requires an async runtime, chosen by feature: tokio (default), async-std, or async-dispatcher. All three are benchmarked.
- Its PUSH/PULL does not multiplex several peers on the bound side, so it cannot fan out or fan in. The project's own README opens by stating it does not implement all of ZeroMQ's feature set. All three of its runtimes are benchmarked as separate variants.
- Source: zeromq/zmq.rs README

### omq 0.21.3

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- Runtime: Offers both: `Context::new().blocking_socket(...)` is a sync socket over OMQ-owned IO threads, and `Context::current()` embeds in an existing tokio runtime.
- All three execution models are benchmarked as separate variants: tokio current-thread, tokio multi-thread, and the synchronous blocking API over library-owned IO threads. That last one is libzmq's model, which makes the pair a direct comparison. Compression transports (lz4, zstd) are an OMQ extension with no libzmq counterpart, so they are outside a comparison benchmark. Note omq documents on_mute as ignored by PUB/XPUB: those sockets are always lossy on mute unless xpub_nodrop is set.
- Source: paddor/omq.rs README (8 stable transports; NULL/PLAIN/CURVE)

### monocoque 0.4.0

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Requires a runtime, chosen at compile time: compio (io_uring, default), tokio, or smol. Sockets are !Send and the runtime is thread-per-core.
- All three runtimes are benchmarked as separate variants (compio/io_uring, tokio, smol). 0.4.0 replaced the CURVE message cipher with the real RFC-26 construction and verified live interop against a CURVE-enabled libzmq, so CURVE moved from present-but-broken to working. PUB is the one socket that takes no SocketOptions: its broadcast path ignores buffer sizes and write coalescing, so only its worker count can be tuned.
- Source: vorjdux/monocoque README and CHANGELOG 0.4.0

### rzmq 0.5.25

- Socket types: declared: REQ, REP, DEALER, ROUTER, PUB, SUB, PUSH, PULL
- Runtime: tokio, with an io_uring path on Linux.
- Every row here is the project's claim. The arena has not run a single cell against rzmq, so nothing in it is verified.
- Source: crates.io/rzmq

### celerity 0.1.1

- Socket types: declared: sans-IO ZMTP 3.1 core
- Runtime: The protocol core is sans-IO, so it can be driven without a runtime; the shipped driver is tokio.
- Every row here is the project's claim. The arena has not run a single cell against celerity. The target also pinned a nonexistent 0.2.0 until it was corrected to 0.1.1, so it could not even resolve, let alone measure.
- Source: crates.io/celerity

