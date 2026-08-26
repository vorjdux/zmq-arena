#!/usr/bin/env bash
# Build one target's image and export its filesystem for the harness to run.
#
# The image is a build and packaging artifact, not a runtime: nothing here
# starts a container at measurement time. `docker export` flattens the image to
# a plain directory, and the orchestrator chroots into it to spawn the target as
# its own direct child, in the cgroup and network namespace it already creates.
# That keeps getrusage, /proc polling and the perf tracepoints working exactly as
# they do for a host-built binary.
#
# Usage: scripts/build-image.sh <target-dir> <name> [isa]
set -euo pipefail

dir=${1:?target directory, e.g. targets/libzmq_cpp_target}
name=${2:?short target name, e.g. libzmq}
isa=${3:-x86-64-v3}

repo=$(cd "$(dirname "$0")/.." && pwd)
tag="zmq-arena/${name}:latest"
rootfs="${repo}/${dir}/rootfs"

echo "== building ${tag} (ISA ${isa})"
docker build --build-arg "ISA=${isa}" -t "${tag}" "${repo}/${dir}"

# The digest identifies the exact filesystem a number was produced from. Recorded
# next to the run so a published result can be reproduced rather than trusted.
digest=$(docker image inspect --format '{{.Id}}' "${tag}")

echo "== exporting filesystem to ${rootfs}"
rm -rf "${rootfs}"
mkdir -p "${rootfs}"
cid=$(docker create "${tag}")
trap 'docker rm -f "${cid}" >/dev/null 2>&1 || true' EXIT
docker export "${cid}" | tar -x -C "${rootfs}"

# chroot needs these to exist as mount points; the harness bind-mounts /proc for
# runtimes that read /proc/self. They are empty directories in the image.
mkdir -p "${rootfs}/proc" "${rootfs}/sys" "${rootfs}/dev" "${rootfs}/tmp"

cat > "${rootfs}/.arena-image.json" <<JSON
{"target": "${name}", "tag": "${tag}", "image_id": "${digest}", "isa": "${isa}"}
JSON

# Verify the exported tree, not just the image. `docker export` flattens the
# filesystem and drops the metadata, and ENV is metadata: a target whose
# interpreter finds its packages through GEM_HOME, PYTHONPATH or JAVA_HOME works
# under `docker run` and fails under chroot. The Dockerfile's own describe check
# cannot catch that, because it runs with the environment still applied.
#
# unshare -r gives an unprivileged user namespace to chroot from, so this works
# without root on a normal dev box.
echo "== verifying the exported tree can run outside the image"
for bin in $(find "${rootfs}/app" -maxdepth 1 -type f -name 'target*' -perm -u+x -printf '/app/%f\n' 2>/dev/null); do
  # --pid --fork so procfs can be mounted, and /proc mounted because that is what
  # the orchestrator does for every run: a managed runtime reads /proc/self and
  # the cgroup memory limits at startup and will not boot without it. Checking
  # under weaker conditions than the real run produces false failures.
  if out=$(unshare -r --mount --pid --fork sh -c \
        "mount -t proc proc '${rootfs}/proc' 2>/dev/null; exec chroot '${rootfs}' '${bin}' describe" 2>&1); then
    echo "   ${bin}: $(echo "${out}" | head -c 90)..."
  else
    echo "ERROR: ${bin} cannot run from the exported filesystem:" >&2
    echo "${out}" | head -3 >&2
    echo "Anything the target needs from the environment must be set inside the" >&2
    echo "launcher script, because ENV does not survive docker export." >&2
    exit 1
  fi
done

echo "== ${name}: $(du -sh "${rootfs}" | cut -f1) at ${rootfs}"
echo "   image ${digest}"
