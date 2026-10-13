# Standard build: release binary + project-root symlink ./plica.
# There is deliberately no install target.

.PHONY: build test check lint run e2e perf notices dist

build:
	cargo build --release
	ln -sfn target/release/plica plica

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings

check: lint test build

run: build
	./plica

# End-to-end pty test (needs python-pexpect + python-pyte).
e2e: build
	python3 -I tests/e2e/pty_harness.py

# Large-file responsiveness (35 MB; needs python-pexpect + python-pyte).
perf: build
	python3 -I tests/e2e/perf.py

# Third-party licence notices for the crates compiled into the binary.
notices:
	python3 -I tools/third_party.py THIRD_PARTY_LICENSES.md

# Release package: dist/plica-VERSION-x86_64-linux.tar.gz and its sha256.
# Only adds/overwrites files; never deletes anything.
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
PKG := plica-$(VERSION)-x86_64-linux
# The packaged binary is a static musl build (runs on any x86-64 Linux;
# needs the musl target: `rust-musl` package or rustup).
MUSL := x86_64-unknown-linux-musl
dist: check notices
	cargo build --release --target $(MUSL)
	mkdir -p dist/$(PKG)
	cp -f target/$(MUSL)/release/plica README.md LICENSE CHANGELOG.md THIRD_PARTY_LICENSES.md docs/plica.1 docs/keys.md dist/$(PKG)/
	tar -C dist -czf dist/$(PKG).tar.gz $(PKG)
	cd dist && sha256sum $(PKG).tar.gz > $(PKG).tar.gz.sha256
	@echo "dist/$(PKG).tar.gz"
