# Apex developer tasks. Every target is a thin wrapper over cargo so the same
# commands work on Linux, macOS and Windows (with GNU make or by running the
# cargo lines directly).

CARGO ?= cargo

.PHONY: build release run test lint fmt check bench package clean doc

build:
	$(CARGO) build --workspace

release:
	$(CARGO) build --release --workspace

run:
	$(CARGO) run --bin apex -- $(ARGS)

test:
	$(CARGO) test --workspace

lint:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

fmt:
	$(CARGO) fmt --all

check: 
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace

bench:
	$(CARGO) bench --workspace

doc:
	$(CARGO) doc --workspace --no-deps

package: release
	./packaging/package.sh

clean:
	$(CARGO) clean
