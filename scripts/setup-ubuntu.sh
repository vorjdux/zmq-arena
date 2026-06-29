#!/usr/bin/env bash
# Provision a host to build and run zmq-arena. Targets Ubuntu 24.04 (noble).
# Installs the toolchain and libzmq, checks the environment, and prints the
# caveats that apply when the host is a VM or single-core (e.g. a Linode).
#
# Run from the repo root:  bash scripts/setup-ubuntu.sh
set -euo pipefail

say() { printf '\n== %s ==\n' "$1"; }
warn() { printf 'WARNING: %s\n' "$1" >&2; }

say "host"
. /etc/os-release 2>/dev/null || true
echo "distro:  ${PRETTY_NAME:-unknown}"
echo "kernel:  $(uname -r)"
NPROC="$(nproc)"
echo "vcpus:   ${NPROC}"
VIRT="$(systemd-detect-virt 2>/dev/null || echo unknown)"
echo "virt:    ${VIRT}"

say "packages (sudo apt)"
# build-essential + clang for the C++ target; cmake/pkg-config to find libzmq;
# libzmq3-dev is the libzmq target's link dependency; python3 runs the render
# and sample scripts; curl/git for rustup and sources.
sudo apt-get update -y
sudo apt-get install -y --no-install-recommends \
  build-essential clang cmake pkg-config libzmq3-dev python3 git curl ca-certificates

say "rust toolchain"
if ! command -v cargo >/dev/null 2>&1; then
  echo "installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
# Each project pins its own channel via rust-toolchain.toml; rustup fetches it
# on first build. The orchestrator needs 1.78; omq/rzmq targets need 1.93.
rustup --version || true
echo "cargo:   $(command -v cargo)"

say "cgroup v2"
if [ -f /sys/fs/cgroup/cgroup.controllers ]; then
  echo "unified hierarchy present; controllers: $(cat /sys/fs/cgroup/cgroup.controllers)"
  echo "Note: writing cgroups needs root. Run real isolation with sudo; without"
  echo "root the arena runs functional tests with isolation disabled."
else
  warn "cgroup v2 unified hierarchy not found at /sys/fs/cgroup. Isolation will be skipped."
fi

say "environment caveats"
if [ "${VIRT}" != "none" ] && [ "${VIRT}" != "unknown" ]; then
  warn "This is a ${VIRT} guest. Turbo/CPB and C-states are host-controlled and"
  warn "cannot be locked from inside the VM, so p99.9 tail numbers are NOT"
  warn "admissible here. Use this host for functional/integration testing only."
fi
if [ "${NPROC}" -lt 2 ]; then
  warn "Single vCPU: producer and consumer share one core, so cpuset pinning is"
  warn "a no-op and throughput reflects time-sharing, not parallel pipeline."
  warn "Use this host to validate harness mechanics; use >=2 vCPU (ideally"
  warn "bare metal) for comparative measurement."
fi

say "next steps"
cat <<'EOF'
  # Build the control plane and the libzmq target (the runnable slice):
  cargo build --release --manifest-path orchestrator/Cargo.toml
  cmake -S targets/libzmq_cpp_target -B targets/libzmq_cpp_target/build -DCMAKE_BUILD_TYPE=Release
  cmake --build targets/libzmq_cpp_target/build --parallel

  # Dry-run the dev matrix (no spawning), then a real functional run:
  ./target/release/zmq-arena run --matrix matrix.linode.json --dry-run
  ./target/release/zmq-arena run --matrix matrix.linode.json --run-id "$(date -u +%F)" --out "scratch/$(date -u +%F)"

  # Render results into the dashboard + RANKING.md:
  python3 scripts/render_results.py --scratch "scratch/$(date -u +%F)" --run-id "$(date -u +%F)" \
    --hardware-cpu "$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | sed 's/^ *//')" \
    --hardware-note "single-vCPU KVM guest; functional test, not admissible tail data"

  # View the dashboard locally:
  (cd docs && python3 -m http.server)   # http://localhost:8000
EOF
echo "setup complete."
