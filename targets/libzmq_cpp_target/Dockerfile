# Build environment for the libzmq C++ target.
#
# The image exists to pin the toolchain, not to run anything: the harness
# exports its filesystem and executes the binary directly on the host, as a
# direct child of the orchestrator. Nothing here runs under a container runtime
# at measurement time. See docs/CONTAINERS.md for why.
#
# Pin the base by digest so "rebuild the image" cannot silently mean "rebuild
# against a different libc".
FROM ubuntu:24.04 AS build

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake pkg-config libzmq3-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# The ISA baseline every target compiles against.
#
# This used to be -march=native, which pins the binary to whichever machine
# compiled it. That is fine when the bench host is also the build host and
# nothing is shipped, but it cannot survive a prebuilt image: a binary built on
# a CI runner would either fault or be tuned for the wrong microarchitecture.
# It was also an asymmetry, because the Rust targets never passed target-cpu at
# all, so the C++ wrapper alone got microarchitecture-specific codegen.
#
# x86-64-v3 is AVX2-era (Haswell and later), which every plausible bench host
# satisfies, and it is applied identically to every target.
ARG ISA=x86-64-v3

COPY CMakeLists.txt /src/CMakeLists.txt
COPY src /src/src
RUN cmake -S /src -B /build -DCMAKE_BUILD_TYPE=Release -DARENA_ISA=${ISA} \
    && cmake --build /build -j"$(nproc)"

# The shipped layer: the binary plus only what it loads at run time. No compiler,
# no headers, no package manager.
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends libzmq5 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/libzmq_target /app/target
# A target must answer `describe` before the harness will schedule it, so a
# broken image fails at build time rather than mid-run.
RUN /app/target describe > /app/describe.json && cat /app/describe.json
