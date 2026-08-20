#!/usr/bin/env bash
# Build every target as an independent project.
#
# Each target owns its toolchain, dependency closure, and release profile, so we
# build them one directory at a time rather than with a single workspace build.
# This mirrors the omq.rs comparison harness, where each bench_peer is a
# standalone build unit (scripts/zmqrs_bench_peer, scripts/rzmq_bench_peer, the
# libzmq C file). Add a target by appending its build command below.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "== orchestrator (control plane) =="
cargo build --release --manifest-path orchestrator/Cargo.toml

build_rust_target() {
  local dir="$1"
  echo "== rust target: $dir =="
  # Separate invocation per directory: independent Cargo.lock, profile, and
  # rust-toolchain.toml. No --workspace, by design. Add --locked here once each
  # target has a committed Cargo.lock (run `cargo generate-lockfile` per target
  # first); the weekly grid should always build --locked for reproducibility.
  ( cd "$dir" && cargo build --release )
}

# An engine that selects its runtime at compile time gets one build per runtime,
# each into its own target dir, because every shipped runtime is a measured
# variant in its own right.
build_runtime_variant() {
  local dir="$1" feature="$2" outdir="$3"
  echo "== rust target: $dir ($feature) =="
  ( cd "$dir" && cargo build --release --no-default-features \
      --features "$feature" --target-dir "$outdir" )
}

# monocoque: compio (io_uring, default), tokio and smol.
build_rust_target targets/monocoque_target
build_runtime_variant targets/monocoque_target tokio target-tokio
build_runtime_variant targets/monocoque_target smol target-smol
# zmq.rs (the `zeromq` crate): tokio, async-std and async-dispatcher.
build_rust_target targets/zeromq_rs_target
build_runtime_variant targets/zeromq_rs_target async-std-rt target-async-std
build_runtime_variant targets/zeromq_rs_target async-dispatcher-rt target-async-dispatcher
# rust-zmq (the `zmq` crate): Rust FFI binding over the system libzmq.
build_rust_target targets/rust_zmq_target
# omq.rs comparison roster. One binary covers all three omq variants
# (current-thread, multi-thread, blocking): it selects them at run time.
build_rust_target targets/omq_tokio_target
build_rust_target targets/rzmq_target          # Linux (io_uring)
build_rust_target targets/celerity_target

echo "== c++ target: targets/libzmq_cpp_target =="
cmake -S targets/libzmq_cpp_target -B targets/libzmq_cpp_target/build -DCMAKE_BUILD_TYPE=Release
cmake --build targets/libzmq_cpp_target/build --parallel

echo "all targets built."
