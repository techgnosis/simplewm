#!/bin/sh
# Run a command in an Alpine container with the project root mounted at /work.
#
#   distro/container.sh              # a shell
#   distro/container.sh apk --help   # a command
#
# Rootless podman: root in the container is you on the host, so files the container
# creates as root show up as yours. Files it gives to other users show up owned by your
# sub-IDs; delete those with `podman unshare rm -rf <path>`.
set -eu

ALPINE=3.24

root=$(cd "$(dirname "$0")/.." && pwd)

tty=
if [ -t 0 ] && [ -t 1 ]; then
    tty=-t
fi

[ $# -gt 0 ] || set -- sh

# simplewm runs the session bus without systemd on it, so podman must not ask systemd
# to set up cgroups or log events.
exec podman --cgroup-manager=cgroupfs --events-backend=file \
    run --rm -i $tty \
    --volume "$root:/work" \
    --workdir /work \
    "docker.io/library/alpine:$ALPINE" \
    "$@"
