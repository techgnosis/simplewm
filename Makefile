.PHONY: all simplewm run tty clean

all: simplewm

simplewm:
	cargo build --release
	cp target/release/simplewm ./simplewm

run: simplewm
	./simplewm

tty: simplewm
	./simplewm --tty

clean:
	cargo clean
	rm -f ./simplewm
