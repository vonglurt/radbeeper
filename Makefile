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
# RadBeeper is the Rust program: `make build` builds it and `make install`
# puts it on PATH. A handful of verbs -- `site`, `recompute`, `window`,
# `--plain`, `--source sim` -- are not ported yet and still run out of the
# one-file script at the root, which `make check` also keeps as a reference
# to compare the exporter's bytes against. Neither is advertised: the
# targets below are the front door, and they are the native build.

CARGO  ?= cargo
PYTHON ?= python3
BIN     = target/release/radbeeper
PREFIX ?= $(HOME)/.local
# Read from the manifest, never typed twice: the monitor's own header prints
# these out of CARGO_PKG_*, and `make play` titles the window with them.
NAME    = $(shell sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1)
VERSION = $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# What `make play` opens when nothing else is asked for: the README's own.
GIF    ?= docs/screenshots/watch-hero.gif

.PHONY: all build test check clippy install uninstall package publish-dry \
        release-check release probe watch sim service \
        py-test py-check py-install promo promo-fast play clean help

all: build

## build: the release binary, target/release/radbeeper
build:
	$(CARGO) build --release
	@printf '  built   $(BIN)\n'

## test: cargo test
test:
	$(CARGO) test --locked

## check: a warning-free build, the tests, clippy's bug lints, and the exporter's bytes against the reference
check:
	RUSTFLAGS="-D warnings" $(CARGO) build --release --locked --all-targets
	$(CARGO) test --locked
	# THE WHOLE SUITE, NOT ONE CLASS. This carried `-k TestSameBytes` from
	# the commit that added that class, and was never widened as the others
	# arrived -- so `make check` ran 6 tests of 209 and said OK. The target's
	# own help has always said "the tests".
	$(PYTHON) -m unittest discover -q -s tests
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
	@# The bump is usually this target's own commit. It is not when the
	@# screen itself shows the version: the recording has to be made
	@# against the version it will be published as, so the manifest is
	@# bumped before the shots and committed with them.
	@git diff --cached --quiet -- Cargo.toml Cargo.lock \
	  && echo "  Cargo.toml already says $(V), and is committed" \
	  || git commit -m "radbeeper $(V)"
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

## sim: the monitor against a synthetic Poisson background, no counter needed
sim:
	./radbeeper --source sim --sim-cpm 400 watch

# py-test: the reference suite -- no hardware, no network. Not in `help`:
# `make check` runs it, and nobody needs to run it by hand.
py-test:
	$(PYTHON) -m unittest discover -q -s tests

# py-check: the reference script parses, then its suite. release-check calls it.
py-check:
	$(PYTHON) -c "import ast;ast.parse(open('radbeeper').read()+chr(10))"
	@$(MAKE) --no-print-directory py-test

# py-install: the one-file script into $(PREFIX)/bin, for a machine with no
# toolchain. `make install` is the one people want.
py-install: py-check
	@mkdir -p "$(PREFIX)/bin"
	install -m 0755 radbeeper "$(PREFIX)/bin/radbeeper"
	@echo "installed $(PREFIX)/bin/radbeeper"

## play: play a recording in a window -- GIF=path picks which
# Software X11 on purpose: mpv's GPU path aborts under virtio with no driver,
# which is what this counter is plugged into. feh if there is no mpv -- it
# shows the first frame only, which is better than nothing and worse than mpv.
play:
	@test -f "$(GIF)" || { echo "no $(GIF) -- run make promo first"; exit 1; }
	mpv --vo=x11 --loop-file=inf --no-osc \
	    --title="$(NAME) $(VERSION) -- $(notdir $(GIF))" "$(GIF)" \
	  || feh --title "$(NAME) $(VERSION)" "$(GIF)"

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
