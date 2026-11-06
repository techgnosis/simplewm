#!/usr/bin/env bash
# Run simplewm on the TTY backend with debug logging for DRM/session, writing to simplewm-debug.log.
set -e
cd "$(dirname "$0")"
make simplewm
RUST_LOG=simplewm=debug,smithay::backend::drm=debug,smithay::backend::session=debug \
    ./simplewm 2&> simplewm-debug.log
