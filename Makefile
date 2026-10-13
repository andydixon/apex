# Apex developer tasks. Every target is a thin wrapper over cargo so the same
# commands work on Linux, macOS and Windows (with GNU make or by running the
# cargo lines directly).

CARGO ?= cargo

.PHONY: build release run test lint fmt check bench fuzz package clean doc guide brew-formula repo

build:
	$(CARGO) build --workspace

release:
	$(CARGO) build --release --workspace
	./packaging/package.sh

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

# PDF user guide (pdflatex) into dist/apex-user-guide.pdf.
guide:
	docs/user-guide/latex/build.sh

fuzz:
	cd fuzz && for t in transactions offsets settings search session; do cargo +nightly fuzz run $$t -- -max_total_time=60; done

package: release
	./packaging/package.sh

# Homebrew formula for the newest tag. The tag must already be pushed: the
# checksum is of GitHub's source tarball for it.
brew-formula:
	@set -e; tag=$$(git describe --tags --abbrev=0); \
	url=https://github.com/andydixon/apex/archive/refs/tags/$$tag.tar.gz; \
	sum=$$(curl -fsSL "$$url" | sha256sum | cut -d' ' -f1) || { echo "cannot fetch $$url (tag pushed?)"; exit 1; }; \
	mkdir -p dist/homebrew; \
	sed -e '/^#/d' -e "s/@TAG@/$$tag/g" -e "s/@SHA256@/$$sum/" packaging/homebrew/apex.rb.in > dist/homebrew/apex.rb; \
	echo "wrote dist/homebrew/apex.rb ($$tag, sha256 $$sum)"

# Build .deb/.rpm/Arch packages for the newest tag and publish to repo.dixon.cx.
repo:
	packaging/repo.sh

clean:
	$(CARGO) clean
