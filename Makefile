.PHONY: all simplewm run install clean audit

all: simplewm

simplewm:
	cargo build --release --locked
	install -m755 target/release/simplewm ./simplewm

run: simplewm
	./simplewm

# Install simplewm to /usr/bin and register it as the only backend of
# xdg-desktop-portal. The .portal file must live in /usr/share (the portal does
# not look anywhere else). Needs sudo for both.
install: simplewm
	sudo install -Dm755 ./simplewm /usr/bin/simplewm
	sudo install -Dm644 simplewm.portal /usr/share/xdg-desktop-portal/portals/simplewm.portal
	install -Dm644 simplewm-portals.conf $(HOME)/.config/xdg-desktop-portal/simplewm-portals.conf

clean:
	cargo clean
	rm -f ./simplewm

# Check Cargo.lock's crates against the OSV / RustSec vulnerability database.
audit:
	./audit.py
