# Forge framework delivery roadmap

**Status:** Proposed sequencing based on the complete repository synthesis. This is a plan, not completed implementation or authority to modify source repositories.

## First proof: one useful workflow, not a new platform

Build a reference `jobs.submit` → `jobs.status` workflow. The first route validates a bounded work request and returns a stable durable receipt; the second makes that acceptance observable. Use a trivial, explicitly documented job kind. Execution, Kafka, scheduling, remote model calls, and deployment orchestration are not prerequisites for proving the first admission/storage seam.

Start with one REST adapter, the shared bridge, one capability, and a real local durable-store adapter. A memory adapter is useful for unit tests but cannot satisfy the durability gate. This is intentionally more meaningful than a health endpoint and smaller than recreating Mission Control.

## Milestones and exit evidence

| Stage | Scope | Exit evidence | Do not add yet |
| --- | --- | --- | --- |
| 0 — Select the salvage | Complete the repository reuse map; identify minimal contracts and licensing/ownership constraints before code copying | Every candidate has a source report, concrete extraction boundary, known limitations, and a validation plan | Wholesale repository merges or renamed application scaffolds |
| 1 — Admission kernel | Canonical header/body, immutable registry, route/version checks, trusted-context authorization, explicit handler binding | Duplicate registration is non-mutating; every entry point validates; denied/unknown/conflicting routes have zero handler effects | HTTP/Kafka in the kernel; generic global service lookup |
| 2 — Smallest durable vertical slice | REST submit/status, jobs policy, real store, explicit composition, stable receipts | Integration tests cover success, same-key replay, changed-body conflict, concurrent duplicate submissions, restart persistence, store failure, and lost-response recovery | Worker execution, remote side effects, extra domains |
| 3 — Prove lifecycle and transport parity | Opt-in HTTP/Kafka starters, validated config, supervised startup/shutdown | Equivalent logical inputs reach the same handler; actual broker redelivery and ordered commits work; partial startup closes resources; bounded shutdown preserves outcomes | Dynamic plugin loading or distributed host orchestration |
| 4 — Execute one job type | Supervised worker, durable claim/completion, cancellation policy, retry budget | Crash/reclaim tests; no lost accepted jobs; state transitions and concurrency controls explicit; status reflects reality | General workflow DSL or unbounded fan-out |
| 5 — Prove extension ergonomics | Add a second small capability and one custom provider override without modifying bridge internals | Small composition-only integration; no mandatory unrelated dependencies; absent/default/override/conflict test matrix passes | Broad starter catalog before the extension contract stabilizes |
| 6 — Package for reuse | Version public contracts, document compatibility/support, publish runnable examples and measured limits | Clean consumer build, contract tests, dependency checks, reproducible demo and release checks | Production claims unsupported by operational evidence |

A stage can be split into small reversible patches. Its exit gate—not a date or amount of generated code—determines readiness for the next stage.

## Verification strategy

**Fast:** capability rules, versioned-body decoding, admission rejection without effects, config merge/validation, registry transactionality, override conflict detection, dependency-graph checks.

**Real boundaries:** HTTP parsing/status mapping; Kafka partition ordering, rebalance/redelivery and commit rules; storage concurrency, cleanup, restart persistence, and ambiguity recovery. Keep test doubles, local real-dependency tests, and deployed-system tests visibly distinct.

**Lifecycle:** start failure after each acquisition step; stop during active requests and poll loops; bounded drain and cancellation; all workers joined before resource destruction. Canceling a caller must not falsely report that an already committed job vanished.

**Developer experience:** from a clean consumer project, install only the necessary starter, register a handler, supply config, and run the application. Add a second capability without central business switches. Inspect the dependency graph to prove optional code remains optional.

No source-repository test result is automatically a pass for the assembled framework. Preserve exact commands, environments, failures, and not-run checks when implementation begins.

Before executing any existing aggregate verifier, inspect it and its called helpers. A command named “verify” may restore tracked files, pull changes, install tools, regenerate sibling repositories, or commit/push. Select explicit non-mutating check commands for the first baseline; isolate intentional generation in a disposable workspace with reviewed output boundaries. Do not run a broad script merely because it appears in a README. These are concrete concerns in the [codegen review](../reviews/forge/forge-codegen.md), not hypothetical permission to alter source.

## Portfolio organization

Keep the public portfolio legible:

```text
docs/reviews/forge/       # one sanitized report per reviewed repository
docs/architecture/       # synthesis, decisions, module contract, editable diagrams
docs/roadmap/            # sequenced evidence-based implementation plan
src/rust/                # actual Rust implementations only when built
src/python/              # actual Python projects, not framework kernel dependencies
src/bash/                # operational scripts with documented scope
.agents/skills/          # reusable agent skills, separate from product code
```

Do not create empty implementation trees to imply progress. Keep private raw evidence, repository access details, credentials, deployment addresses, and source snapshots out of the public portfolio. Link architecture conclusions to the sanitized reports; keep deeper evidence privately traceable.

## Control growth before it starts

Every proposed addition must name a real consumer, the responsibility it owns, the dependency cost it imposes, the smallest public contract, and the tests proving it can be removed or replaced. Prefer one coherent capability over a new catch-all package. Retire duplicate prototypes only after their useful behavior is captured and the replacement is demonstrated; review does not itself authorize deletion or migration.
