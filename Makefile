# Usage:
#   make            # build (debug)
#   make run        # cargo run (debug)
#   make release    # optimized build
#   make test       # run unit + integration tests
#   make fmt        # check & auto‑format
#   make clippy     # full‑feature lint, deny warnings
#   make doc        # build docs
#   make clean      # remove target dir
# ----------------------------------------------------

CARGO        ?= cargo
FEATURES     ?= --all-features
TARGETS      ?= --all-targets
PROFILE      ?= dev             # override with `make PROFILE=release build`
CLIPPY_FLAGS ?= $(TARGETS) $(FEATURES) --workspace --profile dev -- -D warnings
FMT_FLAGS    ?= --all

.PHONY: build release run test clean fmt clippy default

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

clean:
	$(CARGO) clean

fmt:
	$(CARGO) fmt

clippy:
	$(CARGO) clippy $(CLIPPY_FLAGS)

test-flashbots:
	$(CARGO) test --test flashbots_provider_test -- --ignored

tx-submitter:
	$(CARGO) run --bin transaction-submitter
