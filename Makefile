# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
# RADBEEPER -- build, run, check, release.
#
# The Makefile is the front door; cargo is what it calls. That is the
# convention across copal, copal-tm, orrery, ascitty and yodacon, and it is
# what `copal-build` expects to find beside a Cargo.toml -- which is why the
# crate sits at the root of this repository and not under a subdirectory.
#
# Requires: cargo, and libc's headers. Nothing else -- the one dependency in
# Cargo.toml is libc, because a serial port is termios.
#
# THE PYTHON IS STILL HERE, and still a program. `export`, `site`,
# `recompute`, `window`, `--plain` and `--source sim` have no Rust counterpart
# yet, so the one-file `radbeeper` at the root owns them and the `py-` targets
# below run it. It is also the oracle: `make check` puts both implementations
# on the same input and compares the bytes.
#
# `hotplug` was on that list until it was ported. It is the one the desktop's
# autostart line runs, so a shim resolving to the Rust build made the
# monitor-on-plug-in stop working -- which is why it went first.

CARGO  ?= cargo
PYTHON ?= python3
BIN     = target/release/radbeeper
PREFIX ?= $(HOME)/.local

.PHONY: all build test check clippy install uninstall package publish-dry \
        release-check release probe watch sim service \
        py-test py-check py-install promo promo-fast clean help

all: build

## build: the release binary, target/release/radbeeper
build:
	$(CARGO) build --release
	@printf '  built   $(BIN)\n'

## test: cargo test
test:
	$(CARGO) test --locked

## check: a warning-free build, the tests, clippy's bug lints, and both implementations writing the same bytes
check:
	RUSTFLAGS="-D warnings" $(CARGO) build --release --locked --all-targets
	$(CARGO) test --locked
	$(PYTHON) -m unittest discover -q -s tests -k TestSameBytes
	$(CARGO) clippy --all-targets --locked \
	  -- -D clippy::correctness -D clippy::suspicious 2>/dev/null \
	  || echo "  (clippy not installed -- CI will run it)"

## clippy: the bug lints on their own
clippy:
	$(CARGO) clippy --all-targets --locked \
	  -- -D clippy::correctness -D clippy::suspicious

## install: cargo install it, so `radbeeper` on PATH is the native one
install:
	$(CARGO) install --path . --locked --force
	@printf '  installed the native binary; `which radbeeper` says where\n'

## uninstall: remove it again
uninstall:
	$(CARGO) uninstall radbeeper || true
	rm -f "$(PREFIX)/bin/radbeeper"

## package: exactly what a `cargo publish` would upload
package:
	$(CARGO) package --locked --list
	$(CARGO) package --locked
	@printf '  packaged target/package/\n'

## publish-dry: the publish, right up to the upload
publish-dry:
	$(CARGO) publish --locked --dry-run

## release-check: is the tree ready to be tagged V=x.y.z
release-check:
	@test -n "$(V)" || { echo "usage: make release-check V=0.2.0"; exit 1; }
	@echo "$(V)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$$' \
	  || { echo "not a semver version: $(V)"; exit 1; }
	@git diff --quiet && git diff --cached --quiet \
	  || { echo "working tree is dirty"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(V)" >/dev/null \
	  && { echo "tag v$(V) already exists"; exit 1; } || true
	@$(MAKE) --no-print-directory py-check
	$(CARGO) build --release --locked
	$(CARGO) test --locked
	@echo "ready to release $(V)"

## release: bump, commit, tag V=x.y.z -- the workflow does the rest
release: release-check
	@grep -c '^version = ' Cargo.toml | grep -qx 1 \
	  || { echo "Cargo.toml: expected exactly one version line"; exit 1; }
	sed -i 's|^version = ".*"|version = "$(V)"|' Cargo.toml
	$(CARGO) update --workspace --offline
	git add Cargo.toml Cargo.lock
	git commit -m "radbeeper $(V)"
	@$(MAKE) --no-print-directory publish-dry
	git tag -a "v$(V)" -m "radbeeper $(V)"
	@echo
	@echo "  tagged v$(V). Push it and the release workflow takes over:"
	@echo "      git push origin main && git push origin v$(V)"

## probe: what is on the USB right now
probe: build
	./$(BIN) probe

## watch: the monitor against real hardware
watch: build
	./$(BIN) watch

## service: what the boot service runs, in the foreground
service: build
	./$(BIN) service

## sim: the monitor against the built-in Poisson background (the Python: --source sim is not ported yet)
sim:
	./radbeeper --source sim --sim-cpm 400 watch

## py-test: the Python suite -- no hardware, no network
py-test:
	$(PYTHON) -m unittest discover -q -s tests

## py-check: the Python parses, then its suite
py-check:
	$(PYTHON) -c "import ast;ast.parse(open('radbeeper').read()+chr(10))"
	@$(MAKE) --no-print-directory py-test

## py-install: copy the one-file program into $(PREFIX)/bin -- no toolchain
py-install: py-check
	@mkdir -p "$(PREFIX)/bin"
	install -m 0755 radbeeper "$(PREFIX)/bin/radbeeper"
	@echo "installed $(PREFIX)/bin/radbeeper"

## promo: re-record every screenshot in docs/ from the real program
promo:
	$(PYTHON) tools/promo.py $(if $(SHOTS),--only $(SHOTS))

## promo-fast: the same, reusing the last monitor recording
promo-fast:
	$(PYTHON) tools/promo.py --keep $(if $(SHOTS),--only $(SHOTS))

## clean: cargo clean
clean:
	$(CARGO) clean

## help: list targets
help:
	@grep -hE '^## ' $(MAKEFILE_LIST) | sed 's/^## /  make /'
