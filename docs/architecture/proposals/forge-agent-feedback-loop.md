# Agent review and repair loop

**Status: implemented in source on 20 November 2026; final integration validation
and deployment are pending.** Migration15, policy `forge-coding-v5`, the review
worker/DAO flow and browser ABI8 must be rolled out together after the release
gates pass. This document does not establish that a running host has them.
It extends the [coding workflow](../coding-admission.md) and follows
[Forge provider wiring](../../../.agents/skills/wire-forge-providers/SKILL.md).
The focused portal reader is [/design/agent-feedback-loop](/design/agent-feedback-loop).

## Why the workflow changed

The preceding Gemini CI task received eight grouped corrective review items;
the coordinator subsequently handled five source/edge-case corrections and two
workflow/verifier issues. Those fifteen groups are not fifteen atomic bugs, and
there is insufficient finding-level history to calculate an agent fix rate.

The earlier [PR #1](https://github.com/roachmaster/homelab/pull/1) checkpoint
passed its local gates and 28 browser groups before the hosted Rust job exposed
an `Infallible` formatting lint under Rust 1.100.0; the local compiler was
1.97.1. The task had already been accepted. The resulting compiler correction
is now in source, but that correction alone does not prove the final integrated
head has passed its hosted gates. Retain the
[original failing job](https://github.com/roachmaster/homelab/actions/runs/44666628259/job/136057383792)
as dated evidence.

The new lifecycle binds feedback, independent review, publication and acceptance
to the actual candidate. A completed parent remains completed; later corrections
use an explicitly linked repair rather than an older native workspace.

## Implemented flow

An owner can authorize bounded repairs when submitting an Implement task or
resuming an approved plan. The visible checkbox defaults off. The immutable plan,
prior results and pinned execution recipe remain attributable to their attempts.
Automatic repair authorization never grants publication or final acceptance.

```mermaid
sequenceDiagram
    participant O as Owner / independent reviewer
    participant API as Admitted Work service
    participant DB as PostgreSQL DAO
    participant W as Coding worker
    participant G as Selected coding CLI
    participant V as Independent verifier
    participant P as Trusted publisher
    O->>API: Submit Implement or Resume exact plan; explicit repair opt-in
    API->>DB: Freeze root repair budget; commit queued attempt
    DB-->>W: Notify; lease-authorized claim
    W->>G: Immutable plan plus separate source-bound feedback
    G-->>W: Candidate, claimed fixes and focused check reports
    W->>W: Stop process scope; release writer
    W->>DB: Reconcile artifact; queue independent review
    DB-->>V: Claim exact stopped candidate
    V->>V: Fixed checks; actual toolchain; gate/source identities
    alt Source failure and authorized repair budget remains
        V->>DB: Append feedback and admit next repair attempt
        DB-->>W: Repair ready; repeat review cycle
    else Owner decision required or handoff
        V->>DB: Persist findings, disposition and reason
        DB-->>O: Review exact candidate and bounded diagnostics
    else Local checks pass
        V->>DB: awaiting_decision; no automatic publication
        O->>API: Approve reviewed result; explicit finding attestations if needed
        API->>DB: Source/revision-bound approval receipt
        DB-->>P: Exact approved candidate becomes publishable
        P->>P: Nonforce feature branch and draft PR
        P->>DB: Persist remote branch/head/PR receipts
        V->>V: Observe required hosted jobs on that exact head
        V->>DB: Append hosted observation or bound repair findings
    end
    O->>API: Accept after required exact-head hosted checks pass
    API->>DB: Complete task; merge remains separate
```

PostgreSQL is the state and lease authority. Notifications only wake workers;
committed records authorize effects. Both normal publication queueing and the
missed-callback claim path require the same reviewed candidate authorization.
A model's successful final text cannot close a finding, approve publication or
complete a task.

A local pass pauses for an independent admitted owner decision. The Agents reader
shows the exact candidate, check/toolchain, round, finding ledger and handoff
reason. Each unresolved finding has an individually unchecked verification
control. Attesting a resolved finding requires an owner note and the exact
review/task revision; the UI never blanket-verifies unseen IDs. Source changes
clear those choices. Final acceptance stays unavailable while required hosted
checks are pending, failed, missing, stale or attached to a different head.

## Persisted feedback, budgets and authority

Migration `0015_coding_feedback_loop.sql` adds review jobs, immutable feedback,
review budgets, hosted observations and review-bound publication identities.
Feedback version1 records the task/parent attempt, optional approved-plan
identity, source revision, parent result SHA256, reconciliation receipt,
workspace digest, candidate/integration commits, gate-manifest digest, round,
disposition, selected profile and bounded findings.

Finding IDs are stable. Categories distinguish source, infrastructure, quota,
authentication, permission and scope. Diagnostics include a check identity,
path/code, reproduction, expected/actual behavior and evidence digest. Private
feedback artifacts are capped at 64KiB; the public dispatch request remains
16KiB. The public API accepts only open findings; trusted verification or an
explicit source-bound owner attestation establishes closure. An agent receives
neither DB credentials nor publication/account authority.

The owner-selected `forge-repair-v1` policy is frozen for the root repair chain.
Defaults are three repair rounds, two successive rounds without independently
verified progress and one same-artifact infrastructure retry. Explicit owner
repair requests share the same root budget; retries and linked children do not
reset it. New config defaults cannot expand an existing authorization. A zero
round limit disables repair rounds without disabling review. Existing per-attempt
process/storage limits remain; coding jobs have no llama.cpp completion-token cap.

| Observation | Implemented next step |
| --- | --- |
| Reproducible source or configuration failure | Deliver a causal bounded finding batch to the selected agent when owner opt-in and root budget admit it; otherwise pause for the owner. |
| Toolchain-specific failure | Retain the actual compiler/version and diagnostic. An older local pass cannot establish target success. |
| Transient verifier failure | An append-only same-artifact retry within the infrastructure budget; no model repair round. |
| Broken verifier, credentials, permission or required tooling | Preserve evidence and hand off; never weaken checks to accommodate the environment. |
| Scope change | Return to plan revision or an explicit owner decision. Repair authority does not expand. |
| Stagnation or exhausted budget | Preserve candidate, open findings and receipts with an explicit handoff reason. |
| Quota, cancellation, lease loss or uncertain stop | Preserve the checkpoint; prove cleanup before a successor writer. No account rotation, paid-credit fallback or consumer OAuth polling. |

The gate manifest binds fixed commands, required hosted workflow/job identities
and protected gate-source bytes. Changes to CI, check scripts or tests require
independent review. Diagnostic text never supplies an executable check command.
Verified finding closure, rather than an agent claim or changing error string,
establishes progress. Fingerprints omit ephemeral timestamps/PIDs/source SHA;
source identity is bound separately.

## Runner and factory/provider boundaries

`CodingWorkerFactory` injects `ReviewContextProvider`. The provider reads only
feedback authorized by the current attempt lease, validates its exact digest and
source, and supplies it separately from `plan_context`. The runner mounts the
feedback read-only. The original plan note is preserved; stale or missing
feedback prevents execution. Compatible Antigravity continuation remains governed
by the existing receipt-owned session provider, not a global latest-session flag.
Changed task/workspace/profile/binary scope starts fresh.

| Owner / port | Implemented responsibility |
| --- | --- |
| `WorkServiceFactory`, scoped stores and migration15 routines | Header/owner/project admission, exact source/revision/idempotency checks, frozen budgets and atomic successor admission. |
| `ReviewContextProvider` at `CodingWorkerFactory` | Lease-bound immutable feedback acquisition and delivery to the selected runner. |
| `PublicationWorkerFactory` | Compose the existing workspace, supervisor, snapshot, Git/check/GitHub providers, `ReviewedCandidateProvider`, `ReviewWorker` and hosted-check observer. |
| Pure `RepairPolicy` / `decide_repair` | Classify structured observations and choose repair, infrastructure retry, review, plan revision or handoff; no I/O or registry lookups. |
| Independent check and candidate providers | Verify fixed checks in an isolated scope; retain candidate, toolchain, gate digest, structured findings and cleanup proof. |
| Existing Git/GitHub providers | Parent-owned credentials, nonforce publication, expected remote-head checks and read-first recovery of uncertain writes. |
| Rust UI controller | Prepare guarded decisions and project current review state. Generated main JS renders/passes actions; worker JS relays admitted I/O. |

No separate daemon is needed for review: the existing optional publication worker
hosts the injected reviewer and hosted observer. Bootstrap selects the providers;
workers do not access global registries. Agent output is data and claimed fixes,
not trusted review authorization.

## Public decisions and completed-task repairs

All operations use authenticated `POST /v1/dispatch` with version1 Forge headers,
owner/project scope, expected task revision and a stable idempotency key.

| Destination / request | Result |
| --- | --- |
| `ide.work.review.decide` / `work_review_decide` | Approve, repair, escalate, retry infrastructure or cancel against an exact review. Optional owner-attested finding IDs require the matching candidate and a note. |
| `ide.work.repair` / `work_repair` | Start a linked correction from an explicit approved-project commit while preserving the completed parent. |
| `ide.work.publication.retry` / `work_publication_retry` | Admit a new run for a failed publication and exact candidate, preserving old failure/remote receipts. |

A legacy completed parent without a bound review first queues a trusted source
import. The importer verifies that the owner-selected commit exists in the
approved project and records its exact source digest before creating a repair
child. The submission receipt has a null `repair_task_id` and an optional review
ID during that stage; no child UUID is preallocated. The portal keeps the parent
open, polls the pending review and exposes linked navigation only when the real
child ID appears. The private source-import authorization is not a browser field.

Source changes require a newly materialized repair workspace when the candidate
contains reviewed integration or coordinator changes. Preserve old receipt-bound
workspaces. Never silently repair an older base, mutate completed history, force
push, merge main or recreate a PR after an uncertain acknowledgement.

## Validation and rollout status

| Area | Evidence / outstanding qualification |
| --- | --- |
| Rust model/protocol/host/generator | Focused source-bound controls, nullable source-import receipts, approval gates, strict DTOs and checkbox acknowledgement/teardown checks passed during implementation. |
| Backend/runtime | Disposable-database and owned-process regression fixtures cover feedback delivery, review, repair/retry, publication fences and recovery. The final combined run must be recorded against the integrated source. |
| Actual browser acceptance | The existing Agents journey was extended for repair opt-in, explicit finding attestation, owner approval and hosted-pending/green states. Full 28-group acceptance and screenshot review remain required for the final head. |
| Hosted CI | Required `check`, `pr-fast-gate` and `real-browser` jobs must all pass on the delivered commit under their verified workflow identities. Historical results do not qualify new source. |
| Deployment | Not deployed by this implementation. Back up and verify migration15, install matching policy5/API/coding/publication runtime, configure the trusted reviewer and activate the atomic ABI8 asset bundle only after release checks. |
| Real agent pilot | No new included-account repair pilot is claimed by this source implementation. Run an explicitly authorized broken-fixture repair after qualification; review evidence before coordinator source edits. |

Run full browser acceptance through `make ui-qa` from the repository root; see
[its prerequisites and report contract](../../../src/rust/forge-ide-web/tests/browser/README.md).
`make ui-qa-check`, native tests and generated-JS fixtures support that gate but do
not substitute for it. Keep run output and Cargo caches outside the source tree,
record the exact source identity and do not edit the checkout during validation.

Useful operational measures remain original-agent verified closure rate,
coordinator source-edit rate, rounds per finding, repeated-failure rate, time to
required green checks and truthful token/cache telemetry. Preserve proven fixes
in regression tests and relevant skills. This is feedback and retrieval, not
model-weight training.
