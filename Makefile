# Repository entry point. Requires GNU Make, stable Rust + rustfmt/clippy, Python 3.
.DEFAULT_GOAL := help
SHELL := /bin/sh
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
export FORGE_RUST_ROOT := $(ROOT)/src/rust
export PACKAGE ?=
export FEATURES ?=
export ALL_FEATURES ?= 0
export NO_DEFAULT_FEATURES ?= 0
export ARGS ?= --help
export ASM_TARGET ?= lib
export NATIVE ?= 0

.PHONY: help build check fmt fmt-check lint test test-release doc check-deps ci run session asm

help:
	@printf '%s\n' \
	  'Forge Rust — run from the repository root' \
	  '  make build / check     Compile / type-check selected packages and targets' \
	  '  make fmt / fmt-check   Format / verify the entire Rust workspace' \
	  '  make lint              Clippy with warnings denied' \
	  '  make test              Debug tests (including doctests)' \
	  '  make test-release      Release tests (including doctests)' \
	  '  make doc               Rustdoc, no dependencies, warnings denied' \
	  '  make check-deps        Package DAG and optional-feature regression guard' \
	  '  make ci                Fixed full + minimal-feature quality gates; no deploy' \
	  '  make run ARGS="node.info"  Run the Forge terminal (default: --help)' \
	  '  make session           Interactive terminal; Ctrl-D or exit finishes' \
	  '  make asm               Release assembly for forge-kernel library' \
	  '' \
	  'Selections: PACKAGE=forge-terminal FEATURES=disk NO_DEFAULT_FEATURES=1' \
	  '            ALL_FEATURES=1 (exclusive with FEATURES/NO_DEFAULT_FEATURES)' \
	  'Assembly:   PACKAGE=forge-terminal ASM_TARGET=bin:forge [NATIVE=1]' \
	  'run/session always select forge-terminal; ci ignores selection overrides.' \
	  'See src/rust/README.md for defaults, prerequisites and output paths.'

# Values travel through the environment as quoted arguments, not shell source.
# Command/extra arguments in this macro are fixed by the recipes below.
define cargo-selected
	@set -eu; \
	case "$$ALL_FEATURES:$$NO_DEFAULT_FEATURES" in 0:0|0:1|1:0) ;; *) echo 'ALL_FEATURES and NO_DEFAULT_FEATURES must be 0/1 and cannot both be 1' >&2; exit 2;; esac; \
	if [ "$$ALL_FEATURES" = 1 ] && [ -n "$$FEATURES" ]; then echo 'ALL_FEATURES and FEATURES are exclusive' >&2; exit 2; fi; \
	set -- --workspace; \
	if [ -n "$$PACKAGE" ]; then set -- --package "$$PACKAGE"; fi; \
	if [ "$$ALL_FEATURES" = 1 ]; then set -- "$$@" --all-features; fi; \
	if [ "$$NO_DEFAULT_FEATURES" = 1 ]; then set -- "$$@" --no-default-features; fi; \
	if [ -n "$$FEATURES" ]; then set -- "$$@" --features "$$FEATURES"; fi; \
	$(1) cargo $(2) --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked "$$@" $(3)
endef

build:
	$(call cargo-selected,,build,--all-targets)
check:
	$(call cargo-selected,,check,--all-targets)
lint:
	$(call cargo-selected,,clippy,--all-targets -- -D warnings)
test:
	$(call cargo-selected,,test,)
test-release:
	$(call cargo-selected,,test,--release)
doc:
	$(call cargo-selected,RUSTDOCFLAGS="-D warnings",doc,--no-deps)
fmt:
	@cargo fmt --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --all
fmt-check:
	@cargo fmt --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --all -- --check
check-deps:
	@python3 "$$FORGE_RUST_ROOT/scripts/check_dependencies.py"

# Fixed gates: selection overrides cannot accidentally narrow CI.
ci:
	@python3 -B -m unittest discover -s "$$FORGE_RUST_ROOT/scripts/tests" -v
	$(MAKE) fmt-check check-deps
	$(MAKE) build check lint test test-release doc PACKAGE= FEATURES= ALL_FEATURES=1 NO_DEFAULT_FEATURES=0
	$(MAKE) test lint PACKAGE=forge-terminal FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test PACKAGE=forge-local FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-bootstrap-demo FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test PACKAGE=forge-bootstrap-demo FEATURES=messages ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test PACKAGE=forge-bootstrap-demo FEATURES=disk ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test PACKAGE=forge-kernel FEATURES=node ALL_FEATURES=0 NO_DEFAULT_FEATURES=1

session: ARGS =

run session asm:
	@python3 "$$FORGE_RUST_ROOT/scripts/cargo_tools.py" $@
