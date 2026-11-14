#!/bin/sh
# Build the Gram editor (https://gram-editor.com, a fork of Zed) from source for musl,
# linking Alpine's C libraries, into target/gram/release/ (gram: the editor, cli: the
# `gram` command that starts it). build.sh calls it and installs both.
# Run on the host (it re-runs itself in the Alpine container) or in the distro, where
# it builds natively with the user's own rustup (see where.sh).
#
# The source tarball is pinned by version and checksum and kept, unpacked, in
# distro/cache/. In the container Rust comes from the same rustup as simplewm's build
# (cache/rustup); Gram's rust-toolchain.toml picks its own toolchain version, which rustup installs the
# first time. Only redone when something changed: a rebuild with nothing new takes
# seconds, the first build most of an hour.
set -eu

. "$(dirname "$0")/where.sh"

VERSION=3.5.0
SHA256=20fafb030a3646c5fd64727a3c0c801f5a7a9940e3943d661104970b246a26ec

CACHE="$PROJECT/distro/cache"
SRC="$CACHE/gram-$VERSION"
mkdir -p "$CACHE"

if [ -z "$NATIVE" ]; then
    export RUSTUP_HOME="$CACHE/rustup"
    export CARGO_HOME="$CACHE/cargo"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
export CARGO_TARGET_DIR="$PROJECT/target/gram"
# Gram's build asks git for its commit ID. The source sits inside simplewm's checkout, so
# where git is installed (the distro) it would find simplewm's repository and report
# simplewm's commit as Gram's. Stop git's search at the cache folder.
export GIT_CEILING_DIRECTORIES="$CACHE"
# Link the C libraries dynamically (Rust's musl target links statically by default):
# Gram loads Vulkan and libwayland at run time, which a static binary can't.
export RUSTFLAGS="-C target-feature=-crt-static"

echo "== gram: build tools"
if [ -n "$NATIVE" ]; then
    need cc pkg-config curl cmake perl rustup
else
    apk add -q build-base pkgconf curl cmake perl \
        wayland-dev libxkbcommon-dev fontconfig-dev zstd-dev sqlite-dev vulkan-loader-dev
fi

if [ -z "$NATIVE" ] && [ ! -x "$CARGO_HOME/bin/rustup" ]; then
    echo "== gram: installing rustup"
    curl -sSf -o /tmp/rustup-init \
        https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-musl/rustup-init
    chmod +x /tmp/rustup-init
    /tmp/rustup-init -y -q --profile minimal --default-toolchain stable --no-modify-path
fi

if [ ! -e "$SRC/Cargo.lock" ]; then
    echo "== gram: source $VERSION"
    tarball="$CACHE/gram-$VERSION.tar.gz"
    [ -e "$tarball" ] ||
        curl -sSfL -o "$tarball" "https://codeberg.org/GramEditor/gram/archive/$VERSION.tar.gz"
    echo "$SHA256  $tarball" | sha256sum -c -s || {
        echo "gram-$VERSION.tar.gz doesn't match its checksum" >&2
        rm -f "$tarball"
        exit 1
    }
    rm -rf "$SRC" "$CACHE/gram"
    tar -xzf "$tarball" -C "$CACHE"
    mv "$CACHE/gram" "$SRC"
fi

cd "$SRC"
# Wayland only: simplewm has no X11, so Gram's X11 support (always on, not a feature
# the build can turn off) is taken out of its Cargo.toml files. That drops x11rb and xim
# and the editor's link to libxkbcommon-x11. The edits are repeated on every build, so
# they also reach a source unpacked before they existed, but only when a file still needs
# them: rewriting it anyway would make Cargo rebuild the editor. Cargo.lock changes by
# one line (xkbcommon no longer needs as-raw-xcb-connection), edited here too; --locked
# stops the build if anything else would change.
if grep -q '"x11"' crates/gram/Cargo.toml Cargo.toml; then
    sed -i 's/^\(gpui = { workspace = true, features = \["cosmic-text", "wayland", "windows-manifest"\), "x11"\]/\1]/' \
        crates/gram/Cargo.toml
    sed -i 's/^\(xkbcommon = { version = "[^"]*", features = \["wayland"\), "x11"\]/\1]/' \
        Cargo.toml
    awk '/^name = / { pkg = $3 }
        !(pkg == "\"xkbcommon\"" && $0 == " \"as-raw-xcb-connection\",")' \
        Cargo.lock > Cargo.lock.new
    mv Cargo.lock.new Cargo.lock
    if grep -q '"x11"' crates/gram/Cargo.toml Cargo.toml; then
        echo "gram: X11 is still turned on in its Cargo.toml (the patch no longer fits)" >&2
        exit 1
    fi
fi

# Gram's build scripts watch .git/logs/HEAD (for the commit ID, which stays unset: the
# source has no git history). A tarball has no .git, and Cargo counts a missing watched file
# as changed, so it would recompile the editor on every build. An empty one dated 1970
# never looks newer than the last build.
if [ ! -e .git/logs/HEAD ]; then
    mkdir -p .git/logs
    touch -d @0 .git/logs/HEAD
fi

echo "== gram: cargo build"
# mimalloc: Gram's optional memory allocator, in place of musl's, which is slow for a
# program that allocates as much as an editor does.
# Gram's toolchain file also asks for the WebAssembly target and rust-analyzer, which
# only its extension and development tooling use; rustup fetches them anyway.
cargo build --release --locked --package gram --package cli --features gram/mimalloc
