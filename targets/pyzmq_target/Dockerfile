# Build environment for the Python targets: pyzmq (Cython binding to libzmq) and
# pyomq (the same API over the omq Rust core), selected with --variant.
#
# The image pins the interpreter and both packages; it is not a runtime. See
# docs/CONTAINERS.md.
FROM ubuntu:24.04

ARG PYZMQ_VERSION=27.2.0
ARG PYOMQ_VERSION=0.20.1

RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# --break-system-packages: this image has no other Python consumer, and a venv
# would only add a path indirection inside a filesystem that exists to run one
# script.
RUN pip3 install --no-cache-dir --break-system-packages \
      "pyzmq==${PYZMQ_VERSION}" \
 && (pip3 install --no-cache-dir --break-system-packages "pyomq==${PYOMQ_VERSION}" \
     || echo "WARNING: pyomq ${PYOMQ_VERSION} unavailable; the pyomq variant will not run")

COPY target.py /app/target.py
# The harness execs `binary` directly, so the entry point has to be an
# executable, not a script the caller knows to pass to an interpreter.
RUN printf '#!/bin/sh\nexec /usr/bin/python3 /app/target.py "$@"\n' > /app/target \
 && chmod +x /app/target
# exec above matters: without it the shell stays as the process the orchestrator
# tracks, and the peak-RSS and per-process CPU sampling would follow the wrapper
# instead of the interpreter doing the work.

RUN /app/target describe > /app/describe.json && cat /app/describe.json
