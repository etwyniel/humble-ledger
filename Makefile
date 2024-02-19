sources := $(shell find src -type f)

.PHONY: build
build: dist/humble_ledger

dist/humble_ledger: $(sources) Cargo.toml Cargo.lock
	cargo build
	mkdir -p dist
	cp -f target/debug/humble_ledger dist/humble_ledger

.PHONY: run
run: dist/humble_ledger
	./run.sh

.PHONY: clean
clean:
	rm -rf dist
	rm -f humble_ledger.sqlite
	cargo clean
