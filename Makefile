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
# The window is a second crate on purpose -- see gui/Cargo.toml -- so it has
# its own target directory and is never built by a bare `make build`. Iced
# brings several hundred crates and `cargo install radbeeper` brings one.
GUIBIN  = gui/target/release/radbeeper-gui
PREFIX ?= $(HOME)/.local
# Read from the manifest, never typed twice: the monitor's own header prints
# these out of CARGO_PKG_*, and `make play` titles the window with them.
NAME    = $(shell sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1)
VERSION = $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# What `make play` opens when nothing else is asked for: the README's own.
GIF    ?= docs/screenshots/watch-hero.gif
# What `make gui-gif` records, and for how long. Two frames a second played
# back at two frames a second: the instrument updates once a second, so this
# is real time and the clock on it can be read.
GIFOUT ?= docs/screenshots/gui.gif
GIFSECS ?= 24
GIFFPS  ?= 2
# The window is maximised for the recording and put back afterwards. The panel
# drops the log table, then the verdicts, then the axis as it runs out of
# height, so a clip taken in a quarter of a screen is a recording of the panel
# with its best parts missing.
GIFFULL ?= 1
# The workspace the window is moved to for the recording, and switched back
# from afterwards. A screen grab cannot grab a window nobody is looking at,
# and an empty workspace is the only way to be sure nothing is sitting on top
# of it. Empty to record it where it is.
GIFWS ?= 2
# What the README's own download is allowed to cost. guicast re-encodes
# narrower until it fits rather than shipping something over budget.
GIFMAXMB ?= 9
# Played faster than captured, for a long session in a short clip. Equal to
# GIFFPS is real time.
GIFPLAY ?= 2
# How wide the frames are scaled before they become a GIF. The starting
# point, not the answer: GIFMAXMB is what decides.
GIFWIDTH ?= 1100

.PHONY: all build test check clippy install uninstall package publish-dry \
        release-check release release-media release-verify bump probe watch \
        sim service gui gui-build gui-install gui-gif site site-serve \
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

## bump: print the next version -- V=x.y.z overrides, otherwise the minor rises
# THE MINOR, NOT THE PATCH, and worked out rather than typed. A release that
# carries new recordings, a new file format or a new flag is not a patch, and
# the version somebody reads off a screenshot has to be the one that drew it.
# `make release V=1.2.3` still says exactly what it means.
NEXTV = $(shell $(PYTHON) -c "import re;v=re.search(r'^version = \"([0-9]+)\.([0-9]+)\.([0-9]+)', open('Cargo.toml').read(), re.M);print('%s.%d.0' % (v.group(1), int(v.group(2))+1))")
bump:
	@echo "$(VERSION) -> $(if $(V),$(V),$(NEXTV))"

## release-verify: nothing uncommitted, nothing untracked, and in step with the remote
# A RELEASE IS A CLAIM ABOUT A COMMIT. Anything in the working tree that is not
# in it -- a stale screenshot, a half-finished doc, a generated page nobody
# added -- ships in the tarball and not in the repository, and the difference
# only ever surfaces months later when somebody cannot reproduce the build.
release-verify:
	@git rev-parse --is-inside-work-tree >/dev/null 2>&1 \
	  || { echo "not a git checkout"; exit 1; }
	@git diff --quiet || { echo "uncommitted changes:"; git diff --stat; exit 1; }
	@git diff --cached --quiet \
	  || { echo "staged but uncommitted:"; git diff --cached --stat; exit 1; }
	@test -z "$$(git ls-files --others --exclude-standard)" \
	  || { echo "untracked files -- add them or ignore them:"; \
	       git ls-files --others --exclude-standard; exit 1; }
	@git rev-parse --abbrev-ref --symbolic-full-name @{u} >/dev/null 2>&1 && { \
	   git fetch --quiet || true; \
	   test -z "$$(git log @{u}..HEAD --oneline)" \
	     || echo "  note: $$(git log @{u}..HEAD --oneline | wc -l) commit(s) not pushed"; \
	 } || echo "  note: no upstream set for this branch"
	@echo "  tree is clean at $$(git rev-parse --short HEAD)"

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

## release-media: every recording and screenshot, when this machine can make them
# SKIPS WHAT IT CANNOT DO, AND SAYS WHICH. The terminal shots need a counter
# (or a fake one on a pty); the window shots need a compositor and a running
# radbeeper-gui. A release from a headless box is a legitimate thing to want,
# and it should not be a release that silently ships last month's pictures --
# so what was skipped is printed rather than passed over.
release-media:
	@echo "== recordings and screenshots =="
	@command -v grim >/dev/null && test -n "$$WAYLAND_DISPLAY" \
	  && { pgrep -x radbeeper-gui >/dev/null \
	       && $(MAKE) --no-print-directory gui-gif \
	       || echo "  SKIPPED gui.gif -- no radbeeper-gui running (make gui)"; } \
	  || echo "  SKIPPED gui.gif -- no Wayland compositor with grim"
	@$(PYTHON) tools/promo.py $(if $(SHOTS),--only $(SHOTS)) \
	  || echo "  SKIPPED the terminal shots -- promo.py could not run"
	@echo "  media done"

## release: bump, build, test, record, publish the site, commit and tag
#
# ONE COMMAND, AND IT CHECKS ITS OWN WORK. The order is the one that matters:
# the version is bumped BEFORE the recordings, because the panel now prints its
# own version in the corner and a screenshot has to be of the build it is
# published as; the site is rebuilt after that, because it embeds the version;
# and the tree is verified clean at the end, because everything above it writes
# files that belong in the commit.
#
#   make release              0.3.1 -> 0.4.0, the minor rises
#   make release V=1.0.0      say exactly what it is
#   make release NOMEDIA=1    skip the recordings, keep what is on disk
release:
	@# The version is settled first, because everything below it is stamped
	@# with the answer -- the binary, the panel's corner, the site's footer.
	$(eval RV := $(if $(V),$(V),$(NEXTV)))
	@echo "$(RV)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$$' \
	  || { echo "not a semver version: $(RV)"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(RV)" >/dev/null \
	  && { echo "tag v$(RV) already exists"; exit 1; } || true
	@echo "== radbeeper $(VERSION) -> $(RV) =="
	@grep -c '^version = ' Cargo.toml | grep -qx 1 \
	  || { echo "Cargo.toml: expected exactly one version line"; exit 1; }
	sed -i 's|^version = ".*"|version = "$(RV)"|' Cargo.toml
	sed -i 's|^version = ".*"|version = "$(RV)"|' gui/Cargo.toml
	@# The README's own badge line says the version too, and a page that
	@# disagrees with the binary it documents is the cheapest kind of wrong.
	sed -i 's|^MIT · `[0-9][^`]*`|MIT · `$(RV)`|' README.md
	$(CARGO) update --workspace --offline
	@cd gui && $(CARGO) update --workspace --offline >/dev/null 2>&1 || true
	@echo "== build and test =="
	@$(MAKE) --no-print-directory check
	@$(MAKE) --no-print-directory gui-build
	@cd gui && $(CARGO) test --release
	$(if $(NOMEDIA),@echo "== media skipped (NOMEDIA) ==",@$(MAKE) --no-print-directory release-media)
	@echo "== the site =="
	@$(MAKE) --no-print-directory site
	@echo "== commit =="
	git add -A
	@git diff --cached --quiet \
	  && echo "  nothing changed" \
	  || git commit -m "radbeeper $(RV)"
	@$(MAKE) --no-print-directory release-verify
	@$(MAKE) --no-print-directory publish-dry
	git tag -a "v$(RV)" -m "radbeeper $(RV)"
	@echo
	@echo "  tagged v$(RV). Push it and the release workflow takes over:"
	@echo "      git push origin main && git push origin v$(RV)"

## probe: what is on the USB right now
probe: build
	./$(BIN) probe

## watch: the monitor against real hardware
watch: build
	./$(BIN) watch

## gui-build: the window, target/release/radbeeper-gui in gui/
gui-build:
	cd gui && $(CARGO) build --release --locked
	@printf '  built   $(GUIBIN)\n'

## gui: the window, against whatever is serving the stream
# It opens no serial port: start `radbeeper service` first, or `make watch`
# in another terminal, and this attaches to it.
gui: gui-build
	./$(GUIBIN)

## gui-install: put the window on PATH beside radbeeper
gui-install: gui-build
	@mkdir -p "$(PREFIX)/bin"
	install -m 0755 $(GUIBIN) "$(PREFIX)/bin/radbeeper-gui"
	@echo "installed $(PREFIX)/bin/radbeeper-gui"

## gui-gif: re-record docs/screenshots/gui.gif from the running window
# WHY NOT `make promo`. That records a TERMINAL -- it keeps the bytes a
# program writes to a pty, which is exact and tiny and no use at all for a
# window. A Wayland surface has only pixels to keep, so this grabs frames and
# assembles them. The window has to be OPEN and on screen: it is a screen
# grab, and a compositor will not hand over a surface nobody is showing.
#
#   make gui-gif                        24 seconds of it, in real time
#   make gui-gif GIFSECS=300 GIFFPS=1 GIFPLAY=12    five minutes at 12x
#   make gui-gif GIFFULL=              leave the window where it is
gui-gif:
	@pgrep -x radbeeper-gui >/dev/null \
	  || { echo "no radbeeper-gui running -- start it first: make gui"; exit 1; }
	$(PYTHON) tools/guicast.py -o $(GIFOUT) --seconds $(GIFSECS) \
	  --fps $(GIFFPS) --play-fps $(GIFPLAY) --width $(GIFWIDTH) \
	  --max-mb $(GIFMAXMB) $(if $(GIFFULL),--fullscreen,) \
	  $(if $(GIFWS),--workspace $(GIFWS),)

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

## site: the GitHub Pages site -- landing page, lab reports, counter report
# WHAT IS SERVED AT THE ROOT. `index.html` is the landing page, rendered from
# README.md, because somebody arriving from a search needs to be told what this
# is before they are shown a table of counts. `monitor.html` is the counter's
# own report, which is what used to be at the root. Both are committed, so
# Pages serves them with no build step.
site:
	$(PYTHON) tools/landing.py
	@test -d logs && $(PYTHON) radbeeper export --logs logs -o monitor.html \
	  || echo "  (no logs/ -- monitor.html left alone)"

## site-serve: look at it before pushing it
site-serve: site
	@echo "  http://127.0.0.1:8765/"
	$(PYTHON) -m http.server 8765 --bind 127.0.0.1

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
