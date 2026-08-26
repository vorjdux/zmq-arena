# Build environment for the rust-zmq (FFI binding to the system libzmq) target.
#
# The image pins the toolchain; it is not a runtime. `scripts/build-image.sh`
# flattens it to a directory and the orchestrator chroots in and execs, so the
# target runs as its own direct child on the host kernel. See docs/CONTAINERS.md.
#
# Ubuntu 24.04 on both stages so every target in the arena is built against and
# runs on the same libc, rather than each picking up whatever its base image
# happened to ship.
FROM ubuntu:24.04 AS build

ARG RUST_VERSION=1.96.0
# The instruction-set baseline, identical for every target. See the note in
# targets/libzmq_cpp_target/CMakeLists.txt for why this is pinned rather than
# native.
ARG ISA=x86-64-v3
ENV RUSTFLAGS="-C target-cpu=${ISA}"

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates build-essential pkg-config libzmq3-dev \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain ${RUST_VERSION} --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /src
COPY . /src
RUN cargo build --release

FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends libzmq5 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/rust-zmq-target /app/target
# Fail at build time if the wrapper cannot answer `describe`, rather than
# mid-run when it is scheduled.
RUN /app/target describe > /app/describe.json && cat /app/describe.json
