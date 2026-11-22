# CI pipeline: branch and main

**Status: drafted 2026-11-22 from Leonardo's pipeline spec (below). The
imports check is built and wired into `make ci`; the branch/main split of
Jenkins and GitHub Actions is specified here but not yet implemented.**

## 1. The spec

Leonardo's pipeline, in his words (2026-11-22):

> fmt done first. Then we want to run the imports pub use commands check.
> To make sure we are calling the correct in scope structs and instances
> etc. then we compile. We run unit test and components tests. Then the
> in memory integration tests. Then we submit to the next level stage.
> ... If unit test component or any other in memory test pass it gets
> merged into main. Any out of memory test like the headless browser
> testing any db test or nats test etc [occurs] only on main branch
> pipeline build.

Working rules that go with it:

- Agents **can** build and test locally; each agent manages their own
  local cache.
- Trunk-based development: each agent creates **one** feature branch and
  never changes it, always rebasing with main before pushing.
- A branch merges when its in-memory stages pass. Out-of-memory tests
  never gate a branch.

## 2. Current state (measured 2026-11-22)

`make ci` today is **14 cargo configurations**: 1 all-features workspace
pass (build, check, lint, test, test-release, doc — the only place
doc/release tests run; 968 tests) plus 13 minimal no-default-features
profiles. Totals: 15 test runs, 10 Clippy runs, then `ui-check` and
`ui-qa-check` (harness tests only; the real `make ui-qa` is a separate
gate). Measured cost (Jenkins qualification 2026-11-21):
**20m25s cold / 5m30s warm** for `make ci`; the total Jenkins run was
36m28s cold. Warm cost is dominated by the first compile, not by the
number of test runs — cutting combinations saves little warm time.

The two CI systems both violate the spec today:

- **Jenkins** (`deploy/mission-control/jenkins/verify-candidate.py`):
  CHECKS is exactly three, all run pre-merge on candidates: `make ci`,
  `make ui-qa` (headless Chrome — out of memory), and the agent-runner
  Python unittests.
- **GitHub** (`.github/workflows/rust.yml`): runs on push to
  main/validation **and every PR**; installs PostgreSQL 16 and runs the
  DB-backed Python suites pre-merge.
- **GitHub** (`.github/workflows/ide-ui-browser.yml`): triggers on push
  to main/validation **and every PR** ("Backend/docs-only agent PRs also
  require the real-browser check") — real browser QA pre-merge.

## 3. The imports / pub use check

`src/rust/scripts/check_imports.py`, built on the pure-Python Rust AST
(`rust_ast.py`). It runs before compile, so it cannot use cargo or any
compiled helper. Wired into `make ci` directly after `fmt-check`
(`make check-imports` runs it standalone).

Rules:

| Rule | What it checks | Level |
|------|----------------|-------|
| R1 | every import resolves to a real module or item in the workspace (std and third-party crates are unverifiable here and skipped); re-export chains, globs, `self`, `super`, `#[macro_export]` macros and macro-generated items (first-argument heuristic) all resolve | error |
| R2 | every import respects visibility: private, `pub(crate)`, `pub(super)`, `pub(in path)` | error |
| R3 | a `lib.rs` holds only `mod` declarations and `use` statements (the formalization rule) | error |
| R4 | a `pub` item is reachable from its crate root through public modules or `pub use` re-exports | warn |
| R5 | no two bindings share a name in one namespace in one module (cfg-alternatives exempt) | error |
| R6 | a `pub use` does not widen visibility (no re-exporting a non-`pub` item as `pub`) | error |
| R7 | every `mod` declaration has a file behind it (warn when cfg-gated) | error |
| R8 | every `.rs` file is in some module tree (no orphan files) | warn |

Findings on the current tree at wiring time: **0 errors, 0 warnings**.
One real finding was fixed on the way in: `MAX_RENDERED_HTML_BYTES`
in `forge-ide-ui` was `pub` in a private module and used only inside
its own file; it is now `pub(crate)`. The check also established that
`forge-ide-web-host/src/design/` is not dead code — it is compiled by
that crate's `build.rs` via `#[path]`, which is why module assignment
follows `mod` declarations instead of file layout.

## 4. Test tiers

The spec names three in-memory tiers: unit, component, integration.
The workspace does not separate them by binary today — unit tests
(inline `#[cfg(test)]` modules), component tests (sibling
`*_component_tests.rs`, through the port) and contract/BDD tests all
live in the lib test binary, so `cargo test` runs them as one lump.

Judgment call (Leonardo's spec does not dictate the mechanism): split by
cargo target, not by test name. Name filtering is fragile; the target
split is structural.

- **Unit + component**: `cargo test --lib`. Both tiers live in the lib
  test binary and both are in-memory; they run together, fast.
- **In-memory integration**: `cargo test --tests` (the `tests/*.rs`
  contract suites) plus `cargo test --doc` (doctests). Both are
  in-memory.
- **Out of memory** (main pipeline only): `make ui-qa` (headless
  Chrome), the Python suites that need a live database, and any future
  NATS tests. No Rust test in the workspace needs a database, a browser,
  or a network service today; the Python suites need a live-DB audit to
  classify each file (open item, section 7).

## 5. The rationalized matrix

### Branch pipeline (`make ci`, gates the merge)

| Stage | Command | Notes |
|-------|---------|-------|
| 0 | Python unittest discovery on `src/rust/scripts/tests` | fast, no cargo |
| 1 | `make fmt-check` | whole workspace |
| 2 | `make check-imports` | section 3 |
| 3 | `make check-deps` | manifest DAG / feature guard |
| 4 | all-features pass: build, check, lint, test, test-release, doc | the one full pass |
| 5 | minimal profiles (10 test + lint, 1 test-only) | section 6 |
| 6 | `make ui-check`, `make ui-qa-check` | harness tests, in-memory |

### Main pipeline build (`make ci-main`, post-merge only)

`make ci` (re-run on main: cheap warm, catches rebase drift) plus:

- `make ui-qa` — the real headless-browser gate.
- The Python suites that need a live database (agent-runner,
  agent-driver, test_services, studio — pending the audit in section 7).
- Any future NATS tests.

## 6. The minimal-profile cut

Ten minimal profiles stay as test runs because they guard real
cfg-gated code the all-features run never exercises with the feature
off: `forge-terminal` bare/jobs/disk (55 `cfg(feature)` sites),
`forge-local` bare (97), `forge-kernel` node-only, `forge-ide`
bare/remote, `forge-remote-https` bare, `forge-ide-web` bare.

Three combinations downgrade to `check` (compile-only):

- `forge-template` bare — its `cli` feature is an empty marker; the
  binary is already built in the all-features run.
- `forge-bootstrap-demo` messages-only and disk-only — the demo has
  zero `cfg(feature)` sites in its own code; its features only swap
  dependencies, which are tested in their own crates and in the
  all-features run.

Per-combination wall time was **not** measured, so no minutes-saved
figure is claimed. A trial run of the reduced target is needed before
cutting (open item, section 7). The cut is a proposal in this doc, not
yet applied to the Makefile.

Tied decisions, still Leonardo's: demo crates to a single gated job
would remove all three demo combinations outright; removing
`forge-llama` from the workspace would stop it compiling in every
combination.

## 7. What changes where

- **Makefile**: add a `make ci-main` target (`make ci` + `make ui-qa` +
  the live-DB Python suites). Apply the three-combination cut from
  section 6 after the trial run.
- **Jenkins** (`verify-candidate.py`): pre-merge candidate verification
  becomes `make ci` only. The main pipeline build runs `make ci-main`
  as a separate job (or the same job conditioned on branch).
- **GitHub `rust.yml`**: PR runs become `make ci` only (no PostgreSQL,
  no DB-backed suites); the main/validation runs become `make ci-main`.
- **GitHub `ide-ui-browser.yml`**: drop the `pull_request` trigger;
  main/validation only.
- **`make dead-code`**: keeps its own overlapping profile set and is not
  part of `make ci`; out of scope for this revision.

## 8. Open items

- Per-combination wall time (cold/warm) and a trial run of the reduced
  `make ci` before applying the section 6 cut.
- Live-DB audit of the Python suites: which files need a live
  PostgreSQL and which run in-memory.
- The `forge-llama`, demo-crate, and `forge-kernel` decisions
  (crate-dependency-tree.md Todo), which change the matrix further.
