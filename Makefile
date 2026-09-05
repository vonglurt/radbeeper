# radbeeper — all the important targets.
#
# There is nothing to compile. The program is one stdlib Python file, and the
# install is a copy: that is the point of it, on a machine where the package
# manager may be a long way away.

PREFIX  ?= $(HOME)/.local
BINDIR   = $(PREFIX)/bin
PYTHON  ?= python3

.PHONY: all test check install uninstall probe watch sim service help \
        rust rust-install rust-check

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

## rust-check: fmt, clippy where available, and a build
rust-check:
	cd rust && cargo fmt --check 2>/dev/null || true
	cd rust && cargo clippy --release 2>/dev/null || cargo build --release

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
