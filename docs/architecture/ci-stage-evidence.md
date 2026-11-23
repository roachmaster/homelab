# Trusted CI stage evidence

The `forge-sdlc-jenkins-v3` profile records which fixed phase actually failed so
the coordinator can attach a retry to the correct failure event. Compilation,
tests, formatting, lint and browser failures no longer share an opaque
`make ci-verified` outcome. Historical v1/v2 requests and receipts retain their
original commands and interpretation.

The effect-free plan is defined by `event_phases_for_scope` in
[`work_ci_phases.py`](../../deploy/mission-control/agent-runner/work_ci_phases.py).
The installed root-owned verifier mirrors the closed declarations. It never
imports executable code from a candidate checkout to decide the gate. Contract
tests compare the complete mirrored phase data. The gate digest binds phase IDs,
kinds, commands and logical groups as well as source and verifier identity.

Main executes 46 actual commands; a branch executes 45 and omits browser QA and
live-database suites. The complete existing feature matrix remains: all-feature
debug/release tests, each minimal/optional feature selection, lint, formatting,
dependency/import checks, documentation, generated assets and browser-driver
tests. Main also retains real browser journeys and all four backend suites with
zero silent skips. The three logical hosted gate groups are `check`,
`real-browser` and `backend-suites` on main; branches use `source-preflight`,
`check` and `in-memory-backend`. Each phase names its exact hosted group. The
receipt does not claim that an unexecuted aggregate command ran.

All fifteen compilation phases precede Rust test execution. New `test-compile`
and `test-release-compile` targets invoke `cargo test --no-run` for every selected
feature variant. UI/WASM, asset-builder and browser fixture binaries are built
before their checks run. Documentation snippets remain tests: Rustdoc compiles
them while executing the doctest, as described in the
[Cargo test documentation](https://doc.rust-lang.org/cargo/commands/cargo-test.html).
The compilation barrier covers ordinary test executables; it does not pretend
that dynamically compiled documentation examples were precompiled.

The protected result adds these fields:

| Field | Meaning |
| --- | --- |
| `phase_results` | Actual ordered `{index, phase_id, kind, argv, returncode}` executions; each entry agrees exactly with the corresponding `checks` entry. |
| `failure_kind` | Kind of the first actual failing command; signals, timeouts and supervisor unavailability are infrastructure failures. |
| `passed_kinds` | Sorted kinds for which every planned phase passed. A partial prefix cannot close a kind. |

Source changes during checks invalidate all pass witnesses. Unknown supervisor
failures use `verification`; raw stdout, progress text and model statements never
create phase receipts. Existing protected source/request/log hashes and proven
process cleanup remain required. Consumers derive `stage_evidence_digest` from
the protected receipt digest rather than trusting a candidate-supplied digest.

Event budgets belong to a persisted failure episode, not a new build or commit.
The coordinator carries the event identity through retries and agent handoffs.
`phase_id` distinguishes different stages that share a kind. A passed result for
the exact phase can resolve its existing event even if another phase later
fails; a kind-wide closure requires `passed_kinds`. The phase producer does not
create budgets, reset counters, approve source or merge changes.

Verification includes a real isolated Cargo compiler error and a real failing
assertion after successful compilation, malformed/forged result rejection,
feature-matrix coverage and scope isolation. Printed claims cannot change the
classification or produce a pass witness.
