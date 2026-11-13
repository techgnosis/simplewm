.PHONY: all simplewm run vm install clean audit

all: simplewm

simplewm:
	cargo build --release --locked
	install -m755 target/release/simplewm ./simplewm

run: simplewm
	./simplewm

# Boot the distro in QEMU, in a window (build it first with distro/build.sh).
vm:
	distro/run.sh

# Install simplewm to /usr/bin (needs sudo).
install: simplewm
	sudo install -Dm755 ./simplewm /usr/bin/simplewm

clean:
	cargo clean
	rm -f ./simplewm

# Check Cargo.lock's crates against the OSV / RustSec vulnerability database.
audit:
	./audit.py
