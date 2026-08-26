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

echo "== ${name}: $(du -sh "${rootfs}" | cut -f1) at ${rootfs}"
echo "   image ${digest}"
