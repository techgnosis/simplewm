.PHONY: all simplewm run tty install-portal clean

all: simplewm

# The Network applet sets the wired card's address and routes, which needs
# CAP_NET_ADMIN. Replacing the binary drops its capabilities, so it is only
# copied when it changed, and setcap (via sudo) runs only when needed.
# Programs simplewm launches do not get the capability.
simplewm:
	cargo build --release
	cmp -s target/release/simplewm ./simplewm || install -m755 target/release/simplewm ./simplewm
	@getcap ./simplewm | grep -q cap_net_admin || { \
		echo "Giving ./simplewm CAP_NET_ADMIN for the Network applet (sudo setcap)"; \
		sudo setcap cap_net_admin+ep ./simplewm \
			|| echo "warning: setcap failed; the Network applet will not be able to change settings"; \
	}

run: simplewm
	./simplewm

tty: simplewm
	./simplewm --tty

# Register simplewm as the only backend of xdg-desktop-portal. The .portal
# file must live in /usr/share (the portal does not look anywhere else), so this
# asks for sudo for that one file.
install-portal:
	sudo install -Dm644 simplewm.portal /usr/share/xdg-desktop-portal/portals/simplewm.portal
	install -Dm644 simplewm-portals.conf $(HOME)/.config/xdg-desktop-portal/simplewm-portals.conf

clean:
	cargo clean
	rm -f ./simplewm
