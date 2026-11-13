.PHONY: all simplewm run install clean audit

all: simplewm

simplewm:
	cargo build --release --locked
	install -m755 target/release/simplewm ./simplewm

run: simplewm
	./simplewm

# Install simplewm to /usr/bin (needs sudo). simplewm is the desktop portal itself
# now, so this also removes the files that once made it an xdg-desktop-portal backend.
install: simplewm
	sudo install -Dm755 ./simplewm /usr/bin/simplewm
	sudo rm -f /usr/share/xdg-desktop-portal/portals/simplewm.portal
	rm -f $(HOME)/.config/xdg-desktop-portal/simplewm-portals.conf

clean:
	cargo clean
	rm -f ./simplewm

# Check Cargo.lock's crates against the OSV / RustSec vulnerability database.
audit:
	./audit.py
