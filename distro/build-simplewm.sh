#!/bin/sh
# Build simplewm for musl, linking Alpine's C libraries, into target/alpine/release/.
# Run on the host; it re-runs itself in the Alpine container. build.sh calls it.
#
# Rust comes from rustup (Alpine's rust package is older than simplewm's rust-version),
# kept with Cargo's downloads in distro/cache/ so later builds don't fetch them again.
set -eu

if [ ! -e /etc/alpine-release ]; then
    exec "$(dirname "$0")/container.sh" distro/build-simplewm.sh "$@"
fi

export RUSTUP_HOME=/work/distro/cache/rustup
export CARGO_HOME=/work/distro/cache/cargo
export CARGO_TARGET_DIR=/work/target/alpine
export PATH="$CARGO_HOME/bin:$PATH"
# Link the C libraries dynamically (Rust's musl target links statically by default).
export RUSTFLAGS="-C target-feature=-crt-static"

echo "== simplewm: build tools"
apk add -q build-base pkgconf curl \
    mesa-dev libinput-dev libseat-dev libxkbcommon-dev

if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
    echo "== simplewm: installing Rust"
    curl -sSf -o /tmp/rustup-init \
        https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-musl/rustup-init
    chmod +x /tmp/rustup-init
    /tmp/rustup-init -y -q --profile minimal --default-toolchain stable --no-modify-path
fi

echo "== simplewm: cargo build"
cd /work
cargo build --release --locked
