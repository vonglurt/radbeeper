# bleeper — all the important targets.
#
# There is nothing to compile. The program is one stdlib Python file, and the
# install is a copy: that is the point of it, on a machine where the package
# manager may be a long way away.

PREFIX  ?= $(HOME)/.local
BINDIR   = $(PREFIX)/bin
PYTHON  ?= python3

.PHONY: all test check install uninstall probe watch sim service help

all: check

## test: the suite — no hardware, no network
test:
	$(PYTHON) -m unittest discover -q -s tests

## check: syntax, then the suite (what to run before committing)
check:
	$(PYTHON) -c "import ast;ast.parse(open('bleeper').read()+chr(10))"
	@$(MAKE) --no-print-directory test

## install: copy the program into $(BINDIR) — no root, no packages
install: check
	@mkdir -p "$(BINDIR)"
	install -m 0755 bleeper "$(BINDIR)/bleeper"
	@echo "installed $(BINDIR)/bleeper"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; \
	  *) echo "note: $(BINDIR) is not on your PATH" ;; esac

## uninstall: remove it again
uninstall:
	rm -f "$(BINDIR)/bleeper"

## probe: what is on the USB right now
probe:
	./bleeper probe

## watch: the monitor against real hardware
watch:
	./bleeper watch

## sim: the monitor against the built-in Poisson background
sim:
	./bleeper --source sim --sim-cpm 400 watch

## service: what the boot service runs, in the foreground
service:
	./bleeper service

## help: list targets
help:
	@grep -hE '^## ' $(MAKEFILE_LIST) | sed 's/^## /  make /'
