# Sourced after where.sh by build.sh and build-kernel.sh: Alpine's package tool, set up the
# same way in the container and in the distro.
#
# Packages are checked against Alpine's public signing keys for x86_64, committed in
# distro/keys/ (see "Building in the distro" in CLAUDE.md for where they came from), and
# come from the repositories of the container's Alpine release.
#
# In the container apk is Alpine's own. The distro has no apk, so there it's apk.static
# (Alpine's apk-tools-static package: one self-contained program), downloaded into
# distro/cache/ and pinned by version and checksum: it's the one download apk can't check
# itself. Natively, fetches use an empty package database in cache/tools/ (made with
# apk's --usermode, no root), since apk.static won't fetch without one.
#
#   alpine_apk ARGS…        apk with our keys, repositories and download cache
#   alpine_tools PACKAGE…   build tools, into the container itself (natively, no packages:
#                           it only makes cache/tools/'s database)
#   alpine_fetch DIR PKG…   download packages into DIR (prints nothing)

ALPINE=$(sed -n 's/^ALPINE=//p' "$PROJECT/distro/container.sh")
APK_STATIC_VERSION=3.0.9-r0
APK_STATIC_SHA256=0a1a0682375f66a83b7b903f6e4638ccfe312547d50d336b5f2be9fc391281be

APK_CACHE="$PROJECT/distro/cache"
APK_KEYS="$PROJECT/distro/keys"
APK_REPOS="$APK_CACHE/repositories"
APK_TOOLS="$APK_CACHE/tools"
APK_MIRROR="https://dl-cdn.alpinelinux.org/alpine/v$ALPINE"

mkdir -p "$APK_CACHE"
printf '%s\n' "$APK_MIRROR/main" "$APK_MIRROR/community" > "$APK_REPOS"

if [ -n "$NATIVE" ]; then
    APK="$APK_CACHE/apk.static-$APK_STATIC_VERSION"
    if [ ! -x "$APK" ]; then
        echo "== apk.static $APK_STATIC_VERSION"
        pkg="$APK_CACHE/apk-tools-static-$APK_STATIC_VERSION.apk"
        curl -sSf -o "$pkg.part" "$APK_MIRROR/main/x86_64/apk-tools-static-$APK_STATIC_VERSION.apk"
        echo "$APK_STATIC_SHA256  $pkg.part" | sha256sum -c -s || {
            echo "apk-tools-static-$APK_STATIC_VERSION.apk doesn't match its checksum" >&2
            rm -f "$pkg.part"
            exit 1
        }
        mv "$pkg.part" "$pkg"
        tar -xzOf "$pkg" sbin/apk.static 2>/dev/null > "$APK.part"
        chmod 755 "$APK.part"
        mv "$APK.part" "$APK"
    fi
else
    APK=apk
fi

alpine_apk() {
    "$APK" --keys-dir "$APK_KEYS" --repositories-file "$APK_REPOS" \
        --cache-dir "$APK_CACHE" "$@"
}

alpine_tools() {
    if [ -z "$NATIVE" ]; then
        alpine_apk add --quiet "$@"
    elif [ -e "$APK_TOOLS/lib/apk/db/installed" ]; then
        alpine_apk --root "$APK_TOOLS" add --quiet "$@"
    else
        alpine_apk --root "$APK_TOOLS" --initdb --usermode add --quiet "$@"
    fi
}

alpine_fetch() {
    dir=$1
    shift
    if [ -z "$NATIVE" ]; then
        alpine_apk fetch --quiet --output "$dir" "$@"
    else
        alpine_tools
        alpine_apk --root "$APK_TOOLS" fetch --quiet --output "$dir" "$@"
    fi
}
