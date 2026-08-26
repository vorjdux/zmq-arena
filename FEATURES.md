# Feature matrix

What each implementation supports, next to what it measures. A fast library that lacks the socket type you need, or whose CURVE does not interoperate, is not a candidate no matter where it lands on a chart.

> **This page is curated, not measured.** Every other number in this repo comes out of a run. These rows come from each project's own documentation, read on 2026-08-26. `yes` means the project documents it *and* zmq-arena exercises it; **`declared` means we are repeating the project's claim and have not tested it**. Two of the implementations below are describe-only stubs, so every capability they list is unverified.

Regenerate with `python3 scripts/render_features.py` after editing `features.json`.

## Matrix

| capability | libzmq | rust-zmq | tmq | zmq.rs | omq | monocoque | rzmq | celerity | pyzmq | pyomq | omq.rb | NetMQ | JeroMQ |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| version | 4.3.5 | 0.10 (libzmq 4.3.4) | 0.5.0 (libzmq 4.3.4) | 0.6.0 | 0.21.4 | 0.4.1 | 0.5.25 | 0.1.1 | 27.2.0 | 0.20.1 | 0.28.10 | 4.0.4.3 | 0.6.0 |
| language | C++ | Rust | Rust | Rust | Rust | Rust | Rust | Rust | Python | Python | Ruby | C# | Java |
| implementation | native | FFI to libzmq | FFI to libzmq | native | native | native | native | native | FFI to libzmq | FFI to libzmq | native | native | native |
| socket types | 12 | 12 | 12 | 9 | 11 | 12 | 8 | 4 | 12 | 11 | 11 | 12 | 11 |
| transports | tcp, ipc, inproc, udp, pgm, epgm, tipc, vmci | tcp, ipc, inproc, udp, pgm, epgm | tcp, ipc, inproc, udp, pgm, epgm | tcp, ipc | tcp, ipc, inproc, udp, ws, wss, lz4+tcp, zstd+tcp | tcp, ipc | tcp, ipc, inproc | tcp, ipc | tcp, ipc, inproc, udp, pgm, epgm | tcp, ipc, inproc, udp | tcp, ipc, inproc | tcp, ipc, inproc, pgm, udp | tcp, inproc, ipc |
| NULL | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes |
| PLAIN | declared | declared | declared | no | declared | declared | declared | unknown | declared | declared | unknown | no | declared |
| CURVE | declared | declared | declared | no | declared | declared | partial | declared | declared | declared | unknown | declared | declared |
| usable without an async runtime | yes | yes | no | no | yes | no | no | partial | yes | yes | yes | yes | yes |
| platforms | Linux, macOS, Windows, BSD | Linux, macOS, Windows | Linux, macOS, Windows | Linux, macOS, Windows (tcp only; ipc is unix-only) | Linux, macOS, Windows | Linux (io_uring, 5.6+ for the compio backend), portable via the tokio/smol backends | Linux | Linux, macOS, Windows | Linux, macOS, Windows | Linux, macOS, Windows | Linux, macOS, Windows | Linux, macOS, Windows | Linux, macOS, Windows |
| bindings | Reference implementation; bindings exist for most languages. | Is itself the Rust binding to libzmq. | Is itself an async Rust binding, layered on rust-zmq. | None. | C/C++ ABI, .NET, Go, Java, Lua, Node, Python, Ruby. | None. | None. | None. | Is itself the Python binding to libzmq. | Is itself a Python binding, to the omq Rust core rather than to libzmq. | None; it is the implementation. | None; it is the implementation. | None; it is the implementation. |
| benchmarked here | headline + extended | headline + extended | headline + extended | headline + extended | headline + extended | headline + extended | headline + extended | headline: latency, pubsub; extended | headline + extended | not benchmarked | headline + extended | headline + extended | headline + extended |

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
- Not an engine: an async facade over rust-zmq, which binds libzmq. Capability therefore follows the linked libzmq, and the series exists to isolate binding and async-wrapper overhead against the libzmq and rust-zmq targets. Socket construction matches the tmq peer in the omq.rs comparison harness, which sets no socket options. tmq documents neither transports nor security mechanisms of its own; both follow the linked libzmq, so nothing here is cited to tmq itself.
- Source: cetra3/tmq README and crates.io; capability is the linked libzmq's

### zmq.rs 0.6.0

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, PUSH, PULL
- Runtime: Requires an async runtime, chosen by feature: tokio (default), async-std, or async-dispatcher. All three are benchmarked.
- Its PUSH/PULL does not multiplex several peers on the bound side, so it cannot fan out or fan in. The project's own README opens by stating it does not implement all of ZeroMQ's feature set. All three of its runtimes are benchmarked as separate variants. inproc is recorded as absent rather than undocumented: it appears neither in the README's transport list nor among the crate's transport features.
- Source: zeromq/zmq.rs README

### omq 0.21.4

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- Runtime: Offers both: `Context::new().blocking_socket(...)` is a sync socket over OMQ-owned IO threads, and `Context::current()` embeds in an existing tokio runtime.
- All three execution models are benchmarked as separate variants: tokio current-thread, tokio multi-thread, and the synchronous blocking API over library-owned IO threads. That last one is libzmq's model, which makes the pair a direct comparison. Compression transports (lz4, zstd) are an OMQ extension with no libzmq counterpart, so they are outside a comparison benchmark. Note omq documents on_mute as ignored by PUB/XPUB: those sockets are always lossy on mute unless xpub_nodrop is set. Bindings list corrected by the OMQ maintainer. OMQ.ts targets the browser over ZWS, which is outside a native-transport comparison and is not benchmarked here.
- Source: paddor/omq.rs README (8 stable transports; NULL/PLAIN/CURVE)

### monocoque 0.4.1

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Requires a runtime, chosen at compile time: compio (io_uring, default), tokio, or smol. Sockets are !Send and the runtime is thread-per-core.
- All three runtimes are benchmarked as separate variants (compio/io_uring, tokio, smol). 0.4.0 replaced the CURVE message cipher with the real RFC-26 construction and verified live interop against a CURVE-enabled libzmq, so CURVE moved from present-but-broken to working. PUB is the one socket that takes no SocketOptions: its broadcast path ignores buffer sizes and write coalescing, so only its worker count can be tuned.
- Source: vorjdux/monocoque README and CHANGELOG 0.4.0

### rzmq 0.5.25

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, PUSH, PULL
- Runtime: Tokio, with an optional io_uring session (zero-copy send, multishot receive). Both backends are benchmarked as separate variants.
- Both IO backends are measured: the stock epoll one and the io_uring session, configured the way the rzmq peer in the omq.rs comparison harness configures it. All five patterns run. CURVE is recorded as partial because the project's own documents disagree: core/README.md lists only NULL, PLAIN and Noise_XX under supported mechanisms, while the socket-option list in that same file and API_REFERENCE.md both describe CURVE behind a `curve` feature. Resolving that needs the maintainer, not a reading.
- Source: crates.io/rzmq; socket loop verified against the running engine

### celerity 0.1.1

- Socket types: REQ, REP, PUB, SUB
- Runtime: Sans-IO core with the crate's own Tokio socket adapters, which is what this wrapper drives.
- celerity 0.1.1 has no pipeline pattern: the crate ships PubCore, SubCore, ReqCore and RepCore and no PUSH/PULL, so throughput, fan-out and fan-in have nothing to drive and are not scheduled. The wrapper rejects them rather than substituting a different pattern. Re-checked against the project's own docs on 2026-08-26: ipc is available behind an `ipc` feature, and PLAIN is not mentioned anywhere in the README or FEATURES.md, so it is recorded as undocumented rather than as supported.
- Source: crates.io/celerity; socket loop verified against the running engine

### pyzmq 27.2.0

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Synchronous API over libzmq's own IO threads; asyncio support is optional and not used here.
- A binding, not an implementation: the engine is the same libzmq the C++ target runs, so its numbers answer what ZeroMQ costs from Python rather than how good a ZMTP implementation it is. Capability follows the linked libzmq; the wheels bundle their own build.
- Source: pyzmq API docs (constants re-exported from libzmq); capability is the linked libzmq's

### pyomq 0.20.1

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- Runtime: Synchronous API over the omq Rust core's own IO threads.
- Presented as a drop-in pyzmq replacement, which is what makes the pair useful: identical Python and identical socket calls, libzmq under one and omq's Rust core under the other, so the difference is the engine alone. Its udp support is RADIO/DISH only.
- Source: pyomq README in the omq.rs tree

### omq.rb 0.28.10

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- Runtime: Fiber-based and async-native, but works outside a reactor: a shared IO thread serves callers that are not inside Async.
- Pure Ruby: no libzmq, no FFI, no C extension for the protocol itself, which makes it the arena's first look at what an interpreted implementation costs. Requires Ruby >= 3.3 and is measured with YJIT enabled, which its README names as the intended fast path. inproc is `ruby://` and aliased.
- Source: zeromq/omq.rb README

### NetMQ 4.0.4.3

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR, STREAM
- Runtime: Synchronous API over NetMQ's own poller threads; no async runtime required in the caller.
- Pure C#, no libzmq: its README calls it "a 100% native C# port" of ZeroMQ, so like JeroMQ it reimplements the protocol rather than binding it. PLAIN is recorded as absent on the evidence of the source tree rather than the docs: src/NetMQ/Core/Mechanisms contains only Null and Curve mechanisms. The prose transport page is also stale, listing neither IPC nor UDP while both exist in the source, so the transport row is cited to the source tree.
- Source: NetMQ docs and source tree; the docs and the tree disagree, see notes

### JeroMQ 0.6.0

- Socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- Runtime: Synchronous API over JeroMQ's own IO threads; no async framework required.
- Pure Java, no JNI and no libzmq: its own README calls it a "pure Java implementation of libzmq", based on libzmq 4.1.7, so it tracks libzmq's behaviour without running any of its code. Pure Java, no JNI and no libzmq. Its ipc:// is emulated over tcp://127.0.0.1 and interoperates only with other JeroMQ peers, so an ipc cell measures a loopback TCP socket rather than a unix socket and is not comparable with the other targets' ipc numbers. 0.6.0 is from February 2024 and the project has been quiet since; the version travels with every record so a reader can weigh that.
- Source: zeromq/jeromq README, Features and Unsupported sections

