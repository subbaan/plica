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
dist: check notices
	mkdir -p dist/$(PKG)
	cp -f target/release/plica README.md CHANGELOG.md THIRD_PARTY_LICENSES.md docs/plica.1 docs/keys.md dist/$(PKG)/
	if [ -f LICENSE ]; then cp -f LICENSE dist/$(PKG)/; else echo "note: no LICENSE file yet"; fi
	tar -C dist -czf dist/$(PKG).tar.gz $(PKG)
	cd dist && sha256sum $(PKG).tar.gz > $(PKG).tar.gz.sha256
	@echo "dist/$(PKG).tar.gz"
