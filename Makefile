# radbeeper — all the important targets.
#
# There is nothing to compile. The program is one stdlib Python file, and the
# install is a copy: that is the point of it, on a machine where the package
# manager may be a long way away.

PREFIX  ?= $(HOME)/.local
BINDIR   = $(PREFIX)/bin
PYTHON  ?= python3

.PHONY: all test check install uninstall probe watch sim service help \
        rust rust-install rust-check rust-package rust-publish-dry \
        release-check release

all: check

## test: the suite — no hardware, no network
test:
	$(PYTHON) -m unittest discover -q -s tests

## check: syntax, then the suite (what to run before committing)
check:
	$(PYTHON) -c "import ast;ast.parse(open('radbeeper').read()+chr(10))"
	@$(MAKE) --no-print-directory test

## install: copy the program into $(BINDIR) — no root, no packages
install: check
	@mkdir -p "$(BINDIR)"
	install -m 0755 radbeeper "$(BINDIR)/radbeeper"
	@echo "installed $(BINDIR)/radbeeper"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; \
	  *) echo "note: $(BINDIR) is not on your PATH" ;; esac

## rust: build the native read-side binary (cargo, in rust/)
rust:
	cd rust && cargo build --release
	@printf '  built   rust/target/release/radbeeper\n'

## rust-install: cargo install it, so `radbeeper` on PATH is the native one
rust-install:
	cargo install --path rust --locked
	@printf '  installed the native binary; `which radbeeper` says where\n'

## rust-check: what CI checks — a warning-free build, tests, clippy's bug lints
rust-check:
	cd rust && RUSTFLAGS="-D warnings" cargo build --release --locked
	cd rust && cargo test --locked
	cd rust && cargo clippy --all-targets --locked \
	  -- -D clippy::correctness -D clippy::suspicious 2>/dev/null \
	  || echo "  (clippy not installed — CI will run it)"

## rust-package: exactly what a `cargo publish` would upload
rust-package:
	cd rust && cargo package --locked --list
	cd rust && cargo package --locked
	@printf '  packaged rust/target/package/\n'

## rust-publish-dry: the publish, right up to the upload
rust-publish-dry:
	cd rust && cargo publish --locked --dry-run

## release-check: is the tree ready to be tagged V=x.y.z
release-check:
	@test -n "$(V)" || { echo "usage: make release-check V=0.2.0"; exit 1; }
	@echo "$(V)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$$' \
	  || { echo "not a semver version: $(V)"; exit 1; }
	@git diff --quiet && git diff --cached --quiet \
	  || { echo "working tree is dirty"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(V)" >/dev/null \
	  && { echo "tag v$(V) already exists"; exit 1; } || true
	@$(MAKE) --no-print-directory check
	cd rust && cargo build --release --locked
	cd rust && cargo test --locked
	@echo "ready to release $(V)"

## release: bump, commit, tag and push V=x.y.z — the workflow does the rest
release: release-check
	@grep -c '^version = ' rust/Cargo.toml | grep -qx 1 \
	  || { echo "rust/Cargo.toml: expected exactly one version line"; exit 1; }
	sed -i 's|^version = ".*"|version = "$(V)"|' rust/Cargo.toml
	cd rust && cargo update --workspace --offline
	git add rust/Cargo.toml rust/Cargo.lock
	git commit -m "radbeeper $(V)"
	@$(MAKE) --no-print-directory rust-publish-dry
	git tag -a "v$(V)" -m "radbeeper $(V)"
	@echo
	@echo "  tagged v$(V). Push it and the release workflow takes over:"
	@echo "      git push origin main && git push origin v$(V)"

## uninstall: remove it again
uninstall:
	rm -f "$(BINDIR)/radbeeper"

## probe: what is on the USB right now
probe:
	./radbeeper probe

## watch: the monitor against real hardware
watch:
	./radbeeper watch

## sim: the monitor against the built-in Poisson background
sim:
	./radbeeper --source sim --sim-cpm 400 watch

## service: what the boot service runs, in the foreground
service:
	./radbeeper service

## help: list targets
help:
	@grep -hE '^## ' $(MAKEFILE_LIST) | sed 's/^## /  make /'
