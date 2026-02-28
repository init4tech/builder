# Usage:
#   make                 # build (debug)
#   make run             # cargo run (debug)
#   make release         # optimized build
#   make test            # run unit + integration tests
#   make test-flashbots  # run flashbots integration tests (ignored tests)
#   make fmt             # check & auto‑format
#   make clippy          # full‑feature lint, deny warnings
#   make doc             # build docs
#   make clean           # remove target dir
# ----------------------------------------------------

CARGO        ?= cargo
FEATURES     ?= --all-features
TARGETS      ?= --all-targets
PROFILE      ?= dev             # override with `make PROFILE=release build`
CLIPPY_FLAGS ?= $(TARGETS) $(FEATURES) --workspace --profile dev -- -D warnings
FMT_FLAGS    ?= --all

.PHONY: build release run test test-flashbots clean fmt clippy default

default: build

#-------------------- Core tasks --------------------

build:
	$(CARGO) build --profile $(PROFILE)

release:
	$(CARGO) build --release

run:
	$(CARGO) run --bin zenith-builder-example

test:
	$(CARGO) test

test-flashbots:
	cargo nextest run --run-ignored=only --workspace

clean:
	$(CARGO) clean

fmt:
	$(CARGO) fmt

clippy:
	$(CARGO) clippy $(CLIPPY_FLAGS)
