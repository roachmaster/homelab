# Event-stream stability — 25 November 2026

This report separates observed delays from delivery failures. Its measurements
come from committed workflow/attempt records and genuine Jenkins receipts, not
from the number of notifications or a runner's heartbeat.

## Measured causes

| Boundary | Evidence | Consequence |
| --- | --- | --- |
| Antigravity provider | Nine invocations failed at20:34UTC; most lasted3.5–3.8seconds. Their actual CLI error was `RESOURCE_EXHAUSTED` / `Individual quota reached`. | The old parser reported generic failure, so otherwise valid workflows escalated. |
| External Muse execution | One repair ran440.3minutes without a work checkpoint. After submission, main-drift handling took about one second. | A routine base update requested another external author turn, repeating the slow boundary. |
| Main CI | Jenkins86 took38m45.7s; backend-main consumed27m14.6s, browser journeys2m21.8s. Cache restore and publication together took2m20.7s, with a6.1GB cache hit. | Sequential backend tests accounted for70.3% of the build. Cold compilation was not the principal delay. |
| Portal activation | Latest activation completed4.49seconds after successful main verification. | Deployment was not the long wait. |
| Historical reconciliation | Two retired actions had exact cancelled/never-started receipts but were repeatedly rejected by the historical cleanup reader. | Repeated uncorrelated warnings obscured current failures. No Jenkins build was running for those actions. |

The durable database scans dispatched source refreshes and reviewer work within
seconds in the inspected examples. `NOTIFY` remains a wake-up hint; the
[delivery contract](../architecture/event-stream-delivery-contract.md) does not
promise bounded latency while a worker is synchronously occupied.

## Provider admission and backup model

The configured primary remains Gemini through `agy`. The user-selected backup
is `claude-opus-5-5-high`, also through `agy`, on the same author profile and
account. Versioned `backup_models` configuration is explicit and defaults empty.
An unconfigured model is rejected.

Only a trusted terminal error or bounded stderr can establish quota exhaustion.
A transient429, an ordinary resource error, and successful model prose do not
establish a quota block. The worker emits a strict, safe quota diagnostic and
records a private durable cooldown. Readiness withholds the exhausted model;
the generic worker conservatively withholds its profile while cooled.

The coordinator reacts to a published quota outcome and genuine stopped-source
reconciliation. A completed source-bound action selects the configured backup
for future execution. The original workflow snapshot, approved Plan, old
attempts and their model recipes remain immutable. New attempts record the
actual selected model. Replaying the same recovery cannot admit another turn.

If the backup is quota blocked, the workflow waits for its reported reset instead
of switching models repeatedly. Reset resumptions are capped at20 for that
unresolved provider/model availability episode; code, compilation and test
repair budgets remain separate. A successful response on that same model ends
the availability episode. Authentication, cancellation and unrelated failures
retain their separate handling.

An explicit interactive `/usage` read at21:00UTC reported Gemini weekly9.57% /
five-hour100%, and shared Claude/GPT weekly98.30% / five-hour97.60%. These are
manual observations, not a headless allowance API. `/credits` reported credit
spending disabled. Forge's immutable per-run settings retain
`useG1Credits: false`; no paid-credit fallback or background consumer OAuth
polling is introduced.

## Trusted source preparation

Clean changes on main are merged in a private bare Git quarantine. Preparation
checks the immutable author commit/tree/base, proves ancestry, freezes the
selected integration, and obtains fresh independent review and exact-source CI.
It never edits the author's workspace or treats an earlier approval as approval
of the new integration.

A genuine conflict returns to the original author. Main advancing after CI
instead creates another bounded preparation generation from the same source
artifact; it retains the existing merge event and its20-attempt limit. Both
legacy and content-v2 preparation retain their original protocol.

## Reconciliation and CI

The historical no-process reader accepts only the exact inactive action,
source scope, cancellation intent and never-submitted receipt. A build journal,
protected result, ambiguous source or missing proof rejects this shortcut.
Existing mutation transport validation is unchanged. Pending errors now carry
safe action/error identifiers; repeated identical warnings are bounded.

Main backend execution groups whole test modules into at most four owned
processes, constrained by CPU and memory availability. Each process retains
class/module cleanup. The supervisor keeps the process leader unreaped through
TERM/KILL cleanup and attempts cleanup of every worker even after one failure.
Discovery and execution must contain exactly the same unique test IDs, without
missing tests, duplicate execution or skips. Branch and preflight coverage and
the four required backend suites remain intact. Fifteen redundant discoveries
from imported test-class aliases are removed; all1492 original unique tests are
retained, plus the new regression coverage.

The new routing, quota and cooldown modules are protected in both V3 policy
producers. Deployment must select the matched immutable verifier and controller
together; historical bindings are never resigned against the new helper.

## Qualification boundary

Focused real-Git, isolated PostgreSQL, CLI parser, process-lifecycle and CI-policy
regressions establish the changes above. Combined-suite execution, publication
and live activation require their own exact-source records in `AGENT_WORK.md`
and private deployment evidence. Component test success alone is not a live
rollout or a completed agent task.

Muse still owns its external runner. A lease renewal establishes ownership,
not coding progress. That runner should emit the existing lease-fenced
`forge_external_agent_api.progress` checkpoints; Mission Control cannot infer
its internal execution speed from a heartbeat.
