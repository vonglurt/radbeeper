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
# Rewrite the FIRST `version = "..."` line of a manifest in place, and only
# the first. See the note in `release` for why this is not a sed.
BUMP = $(PYTHON) tools/bumpversion.py
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
        sim service gui gui-build gui-install gui-gif gui-shots site site-py site-serve \
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
	@# THE CLIP STARTS ITS OWN WINDOW IF THERE IS NOT ONE. This used to
	@# require a radbeeper-gui to already be open and skipped with a note
	@# if there was not -- and a note in the middle of a twenty-minute
	@# release is a note nobody reads. v0.4.1 was tagged with the hero clip
	@# from the build BEFORE it: the version in its corner was wrong and
	@# the dial in it was the one whose scale had just been fixed. A
	@# release that quietly ships last build's picture of the thing it
	@# just changed is worse than one that fails.
	@#
	@# `guishots` already opens its own window per theme, for the same
	@# reason, so this is the rule and not the exception.
	@command -v grim >/dev/null && test -n "$$WAYLAND_DISPLAY" \
	  && { pgrep -x radbeeper-gui >/dev/null \
	         || { echo "  no window open -- starting one for the clip"; \
	              ./$(GUIBIN) >/dev/null 2>&1 & sleep 25; }; \
	       pgrep -x radbeeper-gui >/dev/null \
	         && $(MAKE) --no-print-directory gui-gif \
	         || echo "  SKIPPED gui.gif -- the window would not start"; } \
	  || echo "  SKIPPED gui.gif -- no Wayland compositor with grim"
	@command -v grim >/dev/null && test -n "$$WAYLAND_DISPLAY" \
	  && $(PYTHON) tools/guishots.py $(if $(GIFWS),--workspace $(GIFWS),) \
	  || echo "  SKIPPED the theme shots -- no Wayland compositor with grim"
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
	@# TEST BEFORE BUMPING, and this order was learnt the hard way. The
	@# bump used to come first, so that the recordings below would carry
	@# the version they are published as -- correct, and it meant a failing
	@# test left the tree half-bumped. Worse: `NEXTV` is read from
	@# Cargo.toml when make starts, so the NEXT attempt saw 0.4.0 and
	@# offered 0.5.0. A release that skips a version because a test failed
	@# once is a trap nobody would guess at.
	@#
	@# So the suite runs against the tree as it stands, and nothing is
	@# written until it passes. The rebuild after the bump is what stamps
	@# the new version into the binaries the recordings are made from.
	@echo "== test, before anything is written =="
	@$(MAKE) --no-print-directory check
	@echo "== bump =="
	@# ONLY THE FIRST `version =` LINE IN EACH MANIFEST, which is the one
	@# under [package].
	@#
	@# A bare `sed s|...|...|` replaces EVERY match, and gui/Cargo.toml has
	@# a second one: `[dependencies.iced]` carries `version = "0.14"`.
	@# Bumping that alongside the crate's own asked cargo for iced ^0.4.0 --
	@# which exists, is four years old, and has none of the features this
	@# uses -- so the build died on an unknown feature name with nothing in
	@# the message pointing at the manifest that had been rewritten.
	@#
	@# AND NOT `sed '0,/re/'` EITHER, which is how GNU sed bounds a
	@# substitution to the first match and is not how busybox sed does
	@# anything: it accepts the address, matches nothing, and changes no
	@# bytes at all. This is Alpine, sed is busybox, and that is the second
	@# time its sed has cost this repository an afternoon -- see the
	@# `\x` escape note in docs/reference.md.
	$(BUMP) -v $(RV) Cargo.toml
	$(BUMP) -v $(RV) gui/Cargo.toml
	@# AND THE REFERENCE PROGRAM, which carries its own VERSION and is the
	@# other half of every byte comparison in the suite. Leaving it behind
	@# does not drift quietly: both programs write the version into the
	@# HTML they export, so the whole differential suite fails at once and
	@# points into the middle of index.html. It is what broke v0.4.0's CI.
	$(BUMP) -v $(RV) --python radbeeper
	@# And check it landed where it was meant to, because the failure mode
	@# above was silent in the manifest and loud somewhere else entirely.
	@grep -q '^version = "$(RV)"' Cargo.toml \
	  || { echo "Cargo.toml: the bump did not take"; exit 1; }
	@head -25 gui/Cargo.toml | grep -q '^version = "$(RV)"' \
	  || { echo "gui/Cargo.toml: the bump did not take"; exit 1; }
	@grep -q '^version = "0.14"' gui/Cargo.toml \
	  || { echo "gui/Cargo.toml: iced's version was overwritten"; exit 1; }
	@grep -q '^VERSION = "$(RV)"' radbeeper \
	  || { echo "radbeeper: the reference program was not bumped"; exit 1; }
	@# THE SUITE AGAIN, NOW THAT THE VERSION HAS MOVED. The run above was
	@# against the tree as it stood; this is against what will be tagged,
	@# and it is the one that catches a bump that only landed in half the
	@# places it had to.
	@$(PYTHON) -m unittest discover -q -s tests
	@# The README's own badge line says the version too, and a page that
	@# disagrees with the binary it documents is the cheapest kind of wrong.
	sed -i 's|^MIT · `[0-9][^`]*`|MIT · `$(RV)`|' README.md
	$(CARGO) update --workspace --offline
	@cd gui && $(CARGO) update --workspace --offline >/dev/null 2>&1 || true
	@echo "== rebuild at $(RV) =="
	$(CARGO) build --release --locked
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

## gui-shots: re-take both themes and the squeezed layout
# Each theme is a fresh window: the panel reads the desktop's theme once, at
# startup, so the light shot has to be a window that was started light.
gui-shots: gui-build
	$(PYTHON) tools/guishots.py $(if $(GIFWS),--workspace $(GIFWS),)

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
site: build
	./$(BIN) pages
	@test -d logs && ./$(BIN) export --logs logs -o monitor.html --frames-page \
	  || echo "  (no logs/ -- monitor.html left alone)"

## site-py: the same site, from the stdlib Python -- what the Action runs
# EXCEPT frames.html, WHICH ONLY THE RUST WRITES. `--frames-page` reads the
# .bin frames, which the Python has never decoded; the page it would have to
# produce byte for byte does not exist on this side. So the Action leaves
# whatever `make site` last committed, and the page says so on its own
# footer rather than going quietly stale. See src/browser.rs.
# BYTE FOR BYTE THE SAME PAGES, and tests/test_differential.py is what says
# so. This one needs no toolchain, which is why the workflow uses it and why
# it is not going anywhere.
site-py:
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
