# Sourced by build-gram.sh and build-kernel.sh: where the build runs, and where the
# project is.
#
#   In the Alpine container (/etc/alpine-release): the project is mounted at /work, and
#   the scripts add their tools with apk.
#   In the distro (/sbin/simplewm-init): natively, from wherever the project is. The
#   image already has the tools (the development group in build.sh's PACKAGES).
#   Anywhere else (the Arch host): the script re-runs itself in the container.
#
# Sets PROJECT (the project root) and NATIVE (1 in the distro, empty in the container).

if [ -e /etc/alpine-release ]; then
    NATIVE=
    PROJECT=/work
elif [ -x /sbin/simplewm-init ]; then
    NATIVE=1
    PROJECT=$(cd "$(dirname "$0")/.." && pwd)
else
    exec "$(dirname "$0")/container.sh" "distro/${0##*/}" "$@"
fi

# need COMMAND…: in the distro, stop if one is missing (no apk there to add it).
need() {
    missing=
    for c in "$@"; do
        command -v "$c" >/dev/null 2>&1 || missing="$missing $c"
    done
    if [ -n "$missing" ]; then
        echo "missing:$missing (they come with the distro's development packages)" >&2
        exit 1
    fi
}
