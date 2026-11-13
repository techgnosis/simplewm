.PHONY: all simplewm run debug vm install clean audit

all: simplewm

simplewm:
	cargo build --release --locked

# Nested inside another desktop, or the whole screen on a TTY (chosen automatically).
run: simplewm
	target/release/simplewm

# A debug build, which also logs simplewm's debug messages and smithay's DRM and session
# details, written to simplewm-debug.log.
debug:
	cargo build --locked
	target/debug/simplewm > simplewm-debug.log 2>&1

# Boot the distro in QEMU, in a window (build it first with distro/build.sh).
vm:
	distro/run.sh

# Install simplewm to /usr/bin (needs sudo).
install: simplewm
	sudo install -Dm755 target/release/simplewm /usr/bin/simplewm

clean:
	cargo clean

# Check Cargo.lock's crates against the OSV / RustSec vulnerability database.
audit:
	./audit.py
