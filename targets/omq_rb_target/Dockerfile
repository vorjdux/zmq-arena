# Build environment for omq.rb, the pure-Ruby ZMTP implementation.
#
# No libzmq, no FFI, no C extension for the protocol: omq.rb speaks the wire
# protocol in Ruby. The build tools below are for transitive gems (openssl), not
# for the engine.
#
# This is the one target not on ubuntu:24.04. omq.rb needs Ruby >= 3.3 and 24.04
# ships 3.2, so it uses the official Ruby image rather than a source build that
# would be slower and no more standard. The base differs, and therefore so does
# libc; that matters less here than elsewhere, because the engine under test is
# Ruby bytecode rather than compiled code linked against it. Each target runs in
# its own filesystem, so only self-consistency is required for correctness.
FROM ruby:3.3-slim AS build

ARG OMQ_VERSION=0.28.10

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential libssl-dev pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN gem install omq --version "${OMQ_VERSION}" --no-document

FROM ruby:3.3-slim
COPY --from=build /usr/local/bundle /usr/local/bundle
COPY target.rb /app/target.rb
# exec, so the process the orchestrator tracks is the interpreter doing the work
# rather than a shell the peak-RSS sampling would follow instead.
# --yjit because the engine's own README names it as the intended fast path, and
# measuring a library with its documented optimisation switched off would be
# reporting a configuration nobody deploys. Measured at 25.3k msgs/s without and
# 30.1k with, 64B over TCP. Tuning belongs to the maintainer: if omq.rb's author
# would rather it ran differently, this line is the place to say so.
# The launcher sets its own environment. `docker export` flattens the image's
# filesystem but drops its metadata, and ENV is metadata: GEM_HOME does not
# survive the export, so a chrooted ruby would find no gems. Anything the target
# needs from the environment has to live in this script, not in an ENV line.
RUN printf '#!/bin/sh\nexport GEM_HOME=/usr/local/bundle\nexport GEM_PATH=/usr/local/bundle\nexec /usr/local/bin/ruby --yjit /app/target.rb "$@"\n' > /app/target \
 && chmod +x /app/target
RUN /app/target describe > /app/describe.json && cat /app/describe.json
