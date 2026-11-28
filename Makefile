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
export QA_OUTPUT_ROOT ?=
export CHANGED_ONLY ?= 0
export SCOPE ?= all
export BASE_REF ?= HEAD
export DEAD_CODE_OUT ?=
export AST_OUT ?=
export AST_BASELINE ?=
export INCLUDE_DEV ?= 0
export DRY_RUN ?= 0
export ALLOW_GLOBAL ?= 0
export REFRESH_AST ?= 1
export AFFECTED_PLAN_OUT ?=

.PHONY: help build check fmt fmt-check portal-style-check dead-code ast-yaml rust-ast check-imports build-affected check-affected test-affected lint test test-release test-compile test-release-compile ci-ui-compile doc check-deps ci ci-preflight ci-verified ci-main backend-suites run session template ide remote agent asm ui ui-build ui-check ui-qa ui-qa-check

help:
	@printf '%s\n' \
	  'Forge Rust — run from the repository root' \
	  '  make build / check     Compile / type-check selected packages and targets' \
	  '  make fmt / fmt-check   Format / verify Rust (supports PACKAGE=<pkg> or CHANGED_ONLY=1)' \
	  '  make portal-style-check Verify Rust fmt + portal Python syntax/LF + UI YAML parsing' \
	  '  make dead-code         Layered dead-code check (SCOPE=all|portal, CHANGED_ONLY=0|1)' \
	  '  make ast-yaml          Generate deterministic workspace YAML AST + dependency graph' \
	  '  make rust-ast          Parse all Rust sources (pure Python) and report parse notes' \
	  '  make check-imports     Imports / pub use check (runs before compile)' \
	  '  make build-affected    Rebuild changed + strictly 1-hop direct dependents (no transitive)' \
	  '  make check-affected    Type-check changed + strictly 1-hop direct dependents' \
	  '  make test-affected     Test changed + strictly 1-hop direct dependents (includes dev edges)' \
	  '  make lint              Clippy with warnings denied' \
	  '  make test              Debug tests (including doctests)' \
	  '  make test-release      Release tests (including doctests)' \
	  '  make doc               Rustdoc, no dependencies, warnings denied' \
	  '  make check-deps        Package DAG and optional-feature regression guard' \
	  '  make ci                Fixed full + minimal-feature quality gates; no deploy' \
	  '  make ci-main           Branch gates (make ci) + headless browser QA + live-DB backend suites; main only' \
	  '  make backend-suites    Mission Control backend suites, zero silent skips (main only)' \
	  '  make run ARGS="node.info"  Run the Forge terminal (default: --help)' \
	  '  make session           Interactive terminal; Ctrl-D or exit finishes' \
	  '  make template ARGS="methods"  Run the opt-in template CLI' \
	  '  make ide ARGS="inspect ."  Read a project tree + Rust syntax as YAML' \
	  '  make agent ARGS="--help"  Run the configured remote IDE/node agent' \
	  '  make ui-build          Build real WASM + generated HTML/CSS/JS assets' \
	  '  make ui ARGS="--help"  Run the local IDE web host' \
	  '  make ui-check          Rebuild comparison + real WASM/worker checks' \
	  '  make ui-qa             Build and run sandboxed headless Chrome QA + reports' \
	  '  make ui-qa-check       Test QA reporting/orchestration (not browser evidence)' \
	  '  make remote            One-shot mutual-TLS node.info loopback demo' \
	  '  make asm               Release assembly for forge-system library' \
	  '' \
	  'Selections: PACKAGE=forge-terminal FEATURES=disk NO_DEFAULT_FEATURES=1' \
	  '            ALL_FEATURES=1 (exclusive with FEATURES/NO_DEFAULT_FEATURES)' \
	  'Assembly:   PACKAGE=forge-terminal ASM_TARGET=bin:forge [NATIVE=1]' \
	  'run/session select forge-terminal; template selects forge-template + cli.' \
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
test-compile:
	$(call cargo-selected,,test,--no-run)
test-release-compile:
	$(call cargo-selected,,test,--release --no-run)
ci-ui-compile:
	@cargo build --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked -p forge-ide-web --lib --release --no-default-features --target wasm32-unknown-unknown
	@cargo build --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked -p forge-ide-web --features build-assets --bin forge-ui-assets
	@cargo build --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked -p forge-agent --bin forge-agent
	@cargo build --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked -p forge-ide-web-host --example loopback_fixture
doc:
	$(call cargo-selected,RUSTDOCFLAGS="-D warnings",doc,--no-deps)
fmt:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/style_check.py" fmt
fmt-check:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/style_check.py" fmt-check
portal-style-check:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/style_check.py" portal-style-check
dead-code:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/dead_code.py"
ast-yaml:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/ast_graph.py"
rust-ast:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/rust_ast.py" "$$FORGE_RUST_ROOT" --quiet
check-imports:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/check_imports.py" "$$FORGE_RUST_ROOT"
	@python3 -B "$$FORGE_RUST_ROOT/scripts/check_core_layers.py"
build-affected:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/affected_build.py" build
check-affected:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/affected_build.py" check
test-affected:
	@python3 -B "$$FORGE_RUST_ROOT/scripts/affected_build.py" test
check-deps:
	@python3 "$$FORGE_RUST_ROOT/scripts/check_dependencies.py"

# Fixed gates: selection overrides cannot accidentally narrow CI.
ci:
	@python3 -B -m unittest discover -s "$$FORGE_RUST_ROOT/scripts/tests" -v
	$(MAKE) fmt-check PACKAGE= CHANGED_ONLY=0
	$(MAKE) check-imports
	$(MAKE) check-deps
	$(MAKE) build check lint test test-release doc PACKAGE= FEATURES= ALL_FEATURES=1 NO_DEFAULT_FEATURES=0
	$(MAKE) test lint PACKAGE=forge-remote-https FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-ide FEATURES=remote ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-ide FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-template FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-terminal FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test PACKAGE=forge-local FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-terminal FEATURES=jobs ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-terminal FEATURES=disk ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test PACKAGE=forge-system FEATURES=node ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) test lint PACKAGE=forge-ide-web FEATURES= ALL_FEATURES=0 NO_DEFAULT_FEATURES=1
	$(MAKE) ui-check
	$(MAKE) ui-qa-check

# Run the backend crates against this checkout. The main gate requires the
# disposable PostgreSQL fixture to start and verify the candidate SQL.
backend-suites:
	@set -eu; case "$$SCOPE" in \
	  branch) export FORGE_DISABLE_DISPOSABLE_POSTGRES=1; set -- ;; \
	  main|all) export FORGE_REQUIRE_DISPOSABLE_POSTGRES=1; set -- --all-features ;; \
	  *) echo 'backend-suites requires SCOPE=branch or SCOPE=main' >&2; exit 2 ;; \
	esac; \
	cargo test --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked "$$@" \
		-p forge-host -p forge-work -p forge-work-worker -p forge-work-publication -p forge-postgres \
		-p forge-portal-release-actor -p forge-jenkins -p forge-usage -p forge-agent-driver \
		-p forge-services -p forge-studio -p forge-mcp-db

ci-preflight:
	@FORGE_DISABLE_DISPOSABLE_POSTGRES=1 cargo test --manifest-path "$$FORGE_RUST_ROOT/Cargo.toml" --offline --locked --lib \
		-p forge-work-publication -p forge-work-worker -p forge-jenkins -p forge-portal-release-actor

ci-verified:
	$(MAKE) ci-preflight
	$(MAKE) ci

ci-main:
	$(MAKE) ci-verified
	$(MAKE) ui-qa
	$(MAKE) backend-suites

session: ARGS =

run session template ide remote agent asm:
	@python3 "$$FORGE_RUST_ROOT/scripts/cargo_tools.py" $@

ui:
	@python3 "$$FORGE_RUST_ROOT/scripts/ui_tools.py" serve
ui-build:
	@python3 "$$FORGE_RUST_ROOT/scripts/ui_tools.py" build
ui-check:
	@python3 "$$FORGE_RUST_ROOT/scripts/ui_tools.py" check
ui-qa:
	@python3 "$$FORGE_RUST_ROOT/scripts/ui_qa.py"
ui-qa-check:
	@node --test "$$FORGE_RUST_ROOT/forge-ide-web/tests/browser/"*.test.mjs
