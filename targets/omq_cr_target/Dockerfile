# Build environment for omq.cr, the pure-Crystal ZMTP implementation.
#
# No libzmq, no FFI, no C extension for the protocol: omq.cr speaks the wire
# protocol in Crystal. Crystal compiles to a native binary, so unlike the Ruby
# and Python siblings there is no interpreter to carry into the runtime stage --
# only the binary and the handful of shared libraries Crystal links against.
#
# Like the Ruby target this is not on ubuntu:24.04: omq.cr needs Crystal >= 1.21
# and 24.04 packages nothing recent enough, so it uses the official Crystal
# image. Each target runs in its own exported filesystem, so only
# self-consistency is required for correctness.
FROM crystallang/crystal:1.21-alpine AS build

WORKDIR /src
# The lockfile is committed, and --production refuses to run without one: the
# arena pins every engine it measures, so a rebuilt image gets the same omq.cr
# rather than whatever is newest that day.
COPY shard.yml shard.lock /src/
RUN shards install --production

COPY src /src/src
# --release because the engine's own benchmarks are release builds, and
# measuring a compiled language with optimisation off would report a
# configuration nobody deploys. --static so the runtime stage needs no Crystal
# libraries at all, which keeps the exported rootfs small and self-contained.
RUN shards build target --release --static --no-debug \
    && strip /src/bin/target

FROM alpine:3.20
COPY --from=build /src/bin/target /app/target
RUN chmod +x /app/target
RUN /app/target describe > /app/describe.json && cat /app/describe.json
