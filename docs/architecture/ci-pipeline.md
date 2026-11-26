# CI pipeline: branch and main

**Source status, 2026-11-23:** the explicitly selected
`forge-sdlc-jenkins-v2` contract implements the branch/main split below.
It is not active on Mission Control yet. The last installed native verifier is
`8d0cdefc…`, profile v1: three full legacy commands on every scope. Source v1
keeps that behavior, including database/browser execution. The last release
qualified `95651a5` with native Jenkins #17 and a separate three-suite operator
companion #1. It did not replace the installed helper or activate v2.

The imports/layers checks are wired into `make ci`. `forge-llama` and the demo
crates are retired; `forge-kernel` is now `forge-system`. The `forge-template`
check-only cut remains a proposal.

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
  never changes it, always rebasing with main before pushing. In source
  (`branch_policy = "persistent-agent-v1"`, migration `0039`), each admitted
  agent profile defaults to `forge/agent/<profile-slug>` with exclusive
  active-workflow reservation in `forge_agent.agent_branch_state`, compare-and-swap
  pushes, and correlation-scoped Jenkins CI/CD stage/console/test inspection in
  the portal (ABI 16). Live activation remains a separate authorized step.
- A branch merges when its in-memory stages pass. Out-of-memory tests
  never gate a branch.

## 2. Current state (measured 2026-11-22)

`make ci` today is **11 cargo configurations**: 1 all-features workspace
pass (build, check, lint, test, test-release, doc — the only place
doc/release tests run) plus 10 minimal no-default-features
profiles. Totals: 12 test runs, 9 Clippy runs, then `ui-check` and
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

Nine minimal profiles stay as test runs because they guard real
cfg-gated code the all-features run never exercises with the feature
off: `forge-terminal` bare/jobs/disk (55 `cfg(feature)` sites),
`forge-local` bare (97), `forge-system` node-only, `forge-ide`
bare/remote, `forge-remote-https` bare, `forge-ide-web` bare.

One combination downgrades to `check` (compile-only):

- `forge-template` bare — its `cli` feature is an empty marker; the
  binary is already built in the all-features run.

Per-combination wall time was **not** measured, so no minutes-saved
figure is claimed. A trial run of the reduced target is needed before
cutting (open item, section 7). The cut is a proposal in this doc, not
yet applied to the Makefile.

Decided 2026-11-22: the demo crates and `forge-llama` left the
workspace outright (crate-dependency-tree.md section 9), which removed
all three demo combinations and the llama build from every
configuration. The remaining cut is the single `forge-template` bare
combination above.

## 7. Versioned execution and accounting

`work_ci_contract.py` is the pure typed worker contract. Its factory validates
an explicit profile before creating a scope plan. The root-owned standalone
verifier mirrors the closed command declarations; it never imports candidate
Python to select privileged policy. The producer and both consumers are tested
together against actual supervisor request/result records.

| Profile/scope | Exact execution rows, in order | Logical hosted checks |
|---|---|---|
| v1, every scope | `make ci`; `make ui-qa`; agent-runner unittest discovery | `check`, `real-browser`, `pr-fast-gate` |
| v2 branch/detached | `make ci-preflight`; `make ci`; `python3 -B deploy/mission-control/backend-suites.py --scope branch` | `source-preflight`, `check`, `in-memory-backend` |
| v2 main | `make ci-verified`; `make ui-qa`; `python3 -B deploy/mission-control/backend-suites.py --scope main` | `check`, `real-browser`, `backend-suites` |

The executable paths in protected records are `/usr/bin/make` and
`/usr/bin/python3`. Each scope produces three real command records. This keeps
historical SQL repair envelopes compatible without inventing a browser pass or
a synthetic third command. Main scope requires the candidate to equal the
trusted observed main revision; a pre-merge branch cannot claim a main proof.

`make ci-preflight` runs the fast source/profile/Python boundaries, including
M27 fingerprints and the tiny real delivery/cache E2E, before workspace
compilation. `make ci-verified` runs preflight then `make ci` sequentially.
`make ci-main` runs that target, real browser QA, then all four backend suites.
The main runner rejects disabled PostgreSQL, failures, errors, empty suites and
any skipped test. Main browser QA prepares its real agent/host fixtures.

Branch mode explicitly disables disposable PostgreSQL and records browser and
database coverage as `not_run`. Only skips identified as the declared
PostgreSQL fixture exclusions are allowed there; unexpected skips or zero
executed tests fail. This branch proof is not a full-main proof. Its hosted
names contain no `real-browser` result.

### Explicit bootstrap and immutable identities

Old delivery/publication configurations default to v1. New installations
render v2; `setup-ci.py --native-only` inherits its installed profile unless
`--verification-profile forge-sdlc-jenkins-v2` is explicitly supplied. No
installer, live configuration, job or service is changed by this source fix.

V2 publication configuration explicitly freezes
`jenkins_verification_profile`, `jenkins_verification_scope`, the exact
`checks` vectors and the exact `required_hosted_checks` tuples from the
selected plan. Its separate delivery config must select the same
`verification_profile`. The workflow's frozen repair policy must name that
scope's logical checks as well. An old workflow requiring the old three names
cannot silently become a branch-fast workflow after a config switch.

V1 request/result version, gate hash inputs, transport digest and native action
namespace remain compatible. V2 has distinct request/result and native-binding
versions. Its gate digest additionally binds the actual branch scope and exact
scoped commands. Coverage metadata must match the selected plan. Missing,
extra, reordered, cross-profile or cross-scope command records fail closed.
Inactive historical v1 cleanup evidence remains readable under a replacement
transport, without rewriting history or authorizing an active v2 replay.

New consumed policy files (the backend runner, shared contract and changed
provider/configuration paths) are protected inputs for v2. Both profiles protect the new shared authority file; it is absent from
historical trees, so their v1 source maps remain identical. Other added source
paths remain v2-specific. The compiler compatibility identity includes the complete
v2 execution graph, while storage and gate records retain their own validated
branch scopes; cache reuse cannot upgrade branch coverage into main coverage.

### Source and runtime separation

A deliberate future rollout must qualify and install the matching verifier,
profile and worker configuration together. The [coordinator-owned post-merge
flow](forge-post-merge-verification.md) composes a confirmed merge into a durable,
main-scoped request in source. PostgreSQL records the successor atomically with
the merge receipt; an injected worker in the existing publication process
checkpoints and reconciles the same Jenkins action across restarts. It binds the
actual merged commit/tree, requires the complete v2 main contract and preserves
failed-main follow-up events separately from the completed merge. No service or
broker is added, and the source implementation does not activate live scheduling.
Migration28, matching provider configuration and verifier rollout remain separate.

The GitHub workflow files are separate source artifacts from the earlier
split. This change enables or submits no GitHub Actions workflow. Hosted
labels in v2 Jenkins configuration use `jenkins/<scope>` metadata rather than
pretending to be a GitHub browser job.

## 8. Open items

- Per-combination wall time (cold/warm) and a trial run of the reduced
  `make ci` before applying the section 6 cut.
- Classify remaining service/driver/studio cases before moving any into the
  branch stage. The current branch plan runs only declared agent-runner
  in-memory coverage; main runs all four suites.
- Qualify an intentional v2/migration28 rollout and enable the implemented
  post-merge coordinator with its main-scoped provider. Neither profile rollout
  nor live scheduling is claimed active by this source change.
- The `forge-kernel` rename is done: the crate is now `forge-system`
  (crate-dependency-tree.md section 10). The `forge-llama` and
  demo-crate decisions are done too: both removed from the workspace.
