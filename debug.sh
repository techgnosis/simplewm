#!/usr/bin/env bash
# Run a debug build of simplewm, which logs simplewm's debug messages and smithay's DRM
# and session details, writing to simplewm-debug.log. On a TTY it uses the TTY backend.
set -e
cd "$(dirname "$0")"
cargo build --locked
target/debug/simplewm &> simplewm-debug.log
