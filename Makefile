.PHONY: all simplewm run tty install-portal clean

all: simplewm

simplewm:
	cargo build --release
	cp target/release/simplewm ./simplewm

run: simplewm
	./simplewm

tty: simplewm
	./simplewm --tty

# Register simplewm as the file chooser backend of xdg-desktop-portal. The .portal
# file must live in /usr/share (the portal does not look anywhere else), so this
# asks for sudo for that one file.
install-portal:
	sudo install -Dm644 simplewm.portal /usr/share/xdg-desktop-portal/portals/simplewm.portal
	install -Dm644 simplewm-portals.conf $(HOME)/.config/xdg-desktop-portal/simplewm-portals.conf

clean:
	cargo clean
	rm -f ./simplewm
