#!/bin/sh
# Builds scratch's base image and exports it as a directory for
# FABER_SCRATCH_ROOTFS. With PUSH=1 it also pushes TAG, which then names a
# registry tenantd pulls from (listed in its insecure_registries if it is a
# local one), and prints the reference by digest for FABER_SCRATCH_IMAGE:
# compute and virtual mode must run the same image.
#
# usage: [PUSH=1] export-rootfs.sh DEST [TAG]
#
# The export is written next to DEST and moved into place only when it is
# complete, so a running Faber never reads a half-written base. A session's
# ephemeral layer never outlives its idle, so nothing written over an older
# base survives the update for long.
set -eu

dest=${1:?usage: [PUSH=1] export-rootfs.sh DEST [TAG]}
tag=${2:-faber-scratch-base:latest}
here=$(cd "$(dirname "$0")" && pwd)

docker build --pull -t "$tag" "$here"

staging="$dest.new.$$"
mkdir -p "$staging"
container=$(docker create "$tag")
trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
# --numeric-owner keeps root's files root's, whatever this host's passwd says.
docker export "$container" | tar -C "$staging" --numeric-owner -xf -

# Docker leaves its own mount points as empty files and directories; the
# sandbox mounts its own /proc, /dev and /sys, and these must be plain
# directories for it to.
for point in proc sys dev; do
    [ -d "$staging/$point" ] || mkdir -p "$staging/$point"
done
: > "$staging/etc/hostname"
printf 'nameserver 127.0.0.1\n' > "$staging/etc/resolv.conf"

if [ -e "$dest" ]; then
    mv "$dest" "$dest.old.$$"
fi
mv "$staging" "$dest"
echo "exported $tag to $dest"

if [ "${PUSH:-}" = 1 ]; then
    docker push "$tag" >/dev/null
    echo "FABER_SCRATCH_IMAGE=$(docker inspect --format '{{index .RepoDigests 0}}' "$tag")"
fi
