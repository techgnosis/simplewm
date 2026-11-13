.PHONY: all simplewm run install clean audit

all: simplewm

# The Network applet sets the wired card's address and routes, which needs
# CAP_NET_ADMIN. Replacing the binary drops its capabilities, so it is only
# copied when it changed, and setcap (via sudo) runs only when needed.
# Programs simplewm launches do not get the capability.
simplewm:
	cargo build --release --locked
	cmp -s target/release/simplewm ./simplewm || install -m755 target/release/simplewm ./simplewm
	@getcap ./simplewm | grep -q cap_net_admin || { \
		echo "Giving ./simplewm CAP_NET_ADMIN for the Network applet (sudo setcap)"; \
		sudo setcap cap_net_admin+ep ./simplewm \
			|| echo "warning: setcap failed; the Network applet will not be able to change settings"; \
	}

run: simplewm
	./simplewm

# Install simplewm to /usr/bin (with CAP_NET_ADMIN, as above) and register it
# as the only backend of xdg-desktop-portal. The .portal file must live in
# /usr/share (the portal does not look anywhere else). Needs sudo for both.
install: simplewm
	cmp -s ./simplewm /usr/bin/simplewm || sudo install -Dm755 ./simplewm /usr/bin/simplewm
	@getcap /usr/bin/simplewm | grep -q cap_net_admin || { \
		echo "Giving /usr/bin/simplewm CAP_NET_ADMIN for the Network applet (sudo setcap)"; \
		sudo setcap cap_net_admin+ep /usr/bin/simplewm \
			|| echo "warning: setcap failed; the Network applet will not be able to change settings"; \
	}
	sudo install -Dm644 simplewm.portal /usr/share/xdg-desktop-portal/portals/simplewm.portal
	install -Dm644 simplewm-portals.conf $(HOME)/.config/xdg-desktop-portal/simplewm-portals.conf

clean:
	cargo clean
	rm -f ./simplewm

# Check Cargo.lock's crates against the OSV / RustSec vulnerability database.
audit:
	./audit.py
