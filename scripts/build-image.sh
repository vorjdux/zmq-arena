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

# Reaching the docker socket without sudo is a group membership, and a bench
# host is exactly the kind of machine where that has not been granted. Detect it
# rather than failing with a bare "permission denied" from the daemon, and note
# that root here builds images -- it is not the root the measurement needs,
# which is a separate thing the orchestrator asks for at run time.
#
# Override with DOCKER=... for podman or a rootless socket.
if [ -n "${DOCKER:-}" ]; then
  :
elif docker info >/dev/null 2>&1; then
  DOCKER="docker"
elif sudo -n docker info >/dev/null 2>&1 || sudo docker info >/dev/null 2>&1; then
  DOCKER="sudo docker"
  echo "note: the docker socket needs sudo on this host, so the build uses it."
  echo "      \`sudo usermod -aG docker $USER\` (then re-login) avoids the prompts."
else
  echo "error: cannot reach the docker daemon, with or without sudo." >&2
  echo "       zmq-arena needs docker to BUILD target images. It does not need" >&2
  echo "       docker to run them: see 'Running a measured cell without Docker'" >&2
  echo "       in the README if the images were built elsewhere." >&2
  exit 1
fi

echo "== building ${tag} (ISA ${isa})"
${DOCKER} build --build-arg "ISA=${isa}" -t "${tag}" "${repo}/${dir}"

# The digest identifies the exact filesystem a number was produced from. Recorded
# next to the run so a published result can be reproduced rather than trusted.
digest=$(${DOCKER} image inspect --format '{{.Id}}' "${tag}")

# Everything mounted inside the tree, deepest first. /proc/self/mounts escapes
# the field, so compare the escaped form of the path we are asking about.
mounts_under() {
  local p
  p=$(printf '%s' "$1" | sed 's/ /\\040/g')
  awk -v p="${p}" 'index($2, p"/") == 1 || $2 == p { print $2 }' /proc/self/mounts | sort -r
}

# Never rm -rf a tree that still has something mounted in it. The orchestrator
# bind-mounts /proc into each rootfs and unmounts on the way out, but a run that
# was killed leaves the mount behind -- and then this rm walks a live procfs.
# That is survivable only because procfs refuses to be deleted; any other bind,
# a host directory among them, would be deleted for real. Take the mounts down
# first, and refuse to delete anything if one will not come off.
leftover=$(mounts_under "${rootfs}")
if [ -n "${leftover}" ]; then
  echo "== unmounting leftovers under ${rootfs}"
  while IFS= read -r m; do
    [ -n "${m}" ] || continue
    echo "   ${m}"
    umount "${m}" 2>/dev/null \
      || sudo umount "${m}" 2>/dev/null \
      || sudo umount -l "${m}" 2>/dev/null \
      || true
  done <<< "${leftover}"
fi
still=$(mounts_under "${rootfs}")
if [ -n "${still}" ]; then
  echo "error: something is still mounted under ${rootfs}:" >&2
  printf '       %s\n' ${still} >&2
  echo "       refusing to delete a tree with live mounts. Stop any running" >&2
  echo "       arena process, then re-run." >&2
  exit 1
fi

echo "== exporting filesystem to ${rootfs}"
rm -rf "${rootfs}"
mkdir -p "${rootfs}"
cid=$(${DOCKER} create "${tag}")
trap '${DOCKER} rm -f "${cid}" >/dev/null 2>&1 || true' EXIT
# Extract as root when docker itself needed root: an image filesystem carries
# ownership and modes a normal user cannot reproduce, so a user-mode tar would
# quietly drop them. The tree is handed back afterwards, because everything that
# follows -- the verification, and reading it from the repo -- is unprivileged.
if [ "${DOCKER}" = "docker" ]; then
  ${DOCKER} export "${cid}" | tar -x -C "${rootfs}"
else
  ${DOCKER} export "${cid}" | sudo tar -x -C "${rootfs}"
  sudo chown -R "$(id -u):$(id -g)" "${rootfs}"
fi

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
# chroot needs privilege from somewhere. An unprivileged user namespace is the
# cheapest source and needs no sudo, but it is not always available: Ubuntu 24.04
# and later restrict unprivileged userns through AppArmor by default, so `unshare
# -r` fails there with "write failed /proc/self/uid_map". Fall back to real root,
# which a bench host has anyway -- the orchestrator asks for it at run time, and
# on a host where docker itself needed sudo it has already been granted here.
if [ "$(id -u)" = 0 ]; then
  verify="unshare --mount --pid --fork"
elif unshare -r --mount --pid --fork true 2>/dev/null; then
  verify="unshare -r --mount --pid --fork"
elif sudo -n true 2>/dev/null || [ "${DOCKER}" = "sudo docker" ]; then
  verify="sudo unshare --mount --pid --fork"
else
  verify=""
fi

echo "== verifying the exported tree can run outside the image"
if [ -z "${verify}" ]; then
  # Skipping is a real loss -- this check is what catches a target whose
  # interpreter found its packages through an ENV that docker export dropped --
  # so say so rather than printing nothing and looking like a pass.
  echo "   SKIPPED: no way to chroot here (unprivileged userns is blocked and" >&2
  echo "   sudo is unavailable). The export is unverified: a target that needs" >&2
  echo "   GEM_HOME, PYTHONPATH or JAVA_HOME will fail at run time instead." >&2
  echo "   On Ubuntu 24.04+: sysctl -w kernel.apparmor_restrict_unprivileged_userns=0" >&2
fi
for bin in $([ -n "${verify}" ] && find "${rootfs}/app" -maxdepth 1 -type f -name 'target*' -perm -u+x -printf '/app/%f\n' 2>/dev/null); do
  # --pid --fork so procfs can be mounted, and /proc mounted because that is what
  # the orchestrator does for every run: a managed runtime reads /proc/self and
  # the cgroup memory limits at startup and will not boot without it. Checking
  # under weaker conditions than the real run produces false failures. The mount
  # namespace is why nothing has to be unmounted afterwards.
  if out=$(${verify} sh -c \
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
