# Standard build: release binary + project-root symlink ./plica.
# There is deliberately no install target.

.PHONY: build test check lint run e2e

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
