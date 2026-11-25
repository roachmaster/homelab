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
| Executable identity | Two new Plan invocations stopped in about1.4seconds after AGY updated its user-local binary. | Workers correctly refused the changed program; select the verified immutable1.5.17 installation and recover those exact stopped sources separately. |
| Muse candidate preparation | The submitted candidate tracks two generated Python bytecode files. | The artifact guard rejects them permanently, but the old worker classified that rejection as temporary and retried without author feedback. |
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

The AGY executable is pinned to a verified root-owned versioned installation.
A source-bound private binary-identity recovery must preserve the exact recipe,
Plan stage, workspace and failed attempt; it cannot claim quota exhaustion or a
model timeout. New work still uses the configured primary.

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

Generated/private artifacts remain forbidden. Permanent artifact rejection
returns a bounded source-specific verification repair to the original author,
with safe relative-path diagnostics. Temporary delivery observation failures
retain their separate retry handling.

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
Workers sharing a state root retain each other's cooldown records and apply only
their configured scope. Acquisition creates the private state root before child
providers prepare their directories; no constructor performs I/O. Lock waits and
cache size are bounded.

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

## Observed rollout checkpoint

Source `2ee824e` was pushed to main and its Python services activated at21:57UTC
with matched verifier `3a2b2c238f3186d0`. All five services established fresh
readiness and the authenticated API health check returned200. The exact local
main backend gate passed1568tests across four suites, zero skips/failures, in
10m21s. Its1557 agent-runner IDs match the frozen execution inventory.

Nine quota-stopped tasks resumed through source-bound Opus recovery. Two
pre-execution binary refusals resumed separately with their original recipe.
A real Opus Plan completed, Codex independently requested a revision, and the
coordinator queued it automatically without resetting the existing3/20 event.
The running attempt's settings physically report `useG1Credits: false` and mode
0400. The model's reported token counters remain separate from account allowance.

Muse claimed the original-owner artifact repair at verification event1/20. Its
previous preparation had recorded319 observations of a permanent artifact
rejection. That rejection now becomes actionable feedback rather than another
observation. Claim/renewal alone does not establish completed implementation.

Jenkins87 started through the normal sixty-second source observer. It passed
45checks, including the browser journeys, then its final backend check caught
a cleanup race in the parallel runner: a group descendant could remain briefly
runnable after SIGKILL while the exited leader was reaped. This is genuine failed
CI; release stayed blocked. The follow-up must confirm bounded group quiescence
before reaping the retained leader. No test skip or synthetic success can satisfy
that gate. Final main verification and portal release have separate live receipts.

The follow-up confirms bounded process-group quiescence before reaping its retained
leader. Unreadable, malformed or timed-out observations fail closed. The original
immediate descendant-stop assertion remains strict; a deterministic delayed-kill
regression reproduces the old defect. Thirty focused checks passed under Jenkins
UID990 in its actual mount/PrivateTmp and isolated network environment, with an
independent lifecycle audit. Application service modules remain unchanged; a fresh
automatic main CI result is required for this CI-only follow-up.
