# Agent review and repair loop

**Status: proposed; source review completed 20 November 2026.** This document
does not enable automatic retries, change permissions, or claim deployment.
It extends the [current coding workflow](../coding-admission.md) and follows
[Forge provider wiring](../../../.agents/skills/wire-forge-providers/SKILL.md).

## What the last run demonstrated

Gemini implemented the four CI plan phases and received eight grouped review
items in a corrective continuation. The coordinator subsequently handled five
additional source/edge-case corrections and two workflow/verifier integration
issues. Those fifteen items are grouped findings, not fifteen atomic bugs.
There is insufficient finding-level history to calculate an agent fix rate.

The source is in [PR #1](https://github.com/roachmaster/homelab/pull/1), last
reviewed at `c7b211c61aa0f03ca5fe9db1ef420440f7fbf3d5`. Full local CI,
28 actual browser groups, 290 backend tests and 22 supplemental tests passed;
the backend rerun used a verified PID1 reaper in the isolated verifier.
The native task workspace was preserved and publication used reviewed copies.

The hosted full Rust job subsequently failed on Rust 1.100.0, while the local
compiler was 1.97.1. Formatting the impossible `Singleton::Error = Infallible`
in the [HTTPS example](../../../src/rust/forge-remote-https/examples/loopback.rs)
triggers a newer unreachable-code lint. Browser and fast checks passed.
This is a further source-compatibility finding suitable for Gemini to repair.
[Exact failing hosted job](https://github.com/roachmaster/homelab/actions/runs/44666628259/job/136057383792).

The implementation task was accepted before the hosted result arrived. Future
completion must wait for every required check on the exact delivered head.
Completed history stays immutable; this correction needs an explicitly linked
repair job or a separately designed repair transition, rather than pretending
the completed task can resume.

## Gaps in the current path

| Observed gap | Source and consequence |
| --- | --- |
| Resume feedback is recorded but not delivered as execution context | `WorkStore.resume` stores the note in a resumed event. `CodingWorker` calls `read_plan`, and the runner renders the original plan-decision note. The operator needed a selected runner override to deliver the correction note. |
| No independent repair stage | A coherent implementation result plus reconciliation immediately queues publication. Model completion is not evidence that acceptance checks passed. |
| A callback bypass exists | Publication claim also repairs missed queue callbacks. A review gate must cover both paths. |
| Reviews lack an artifact target | The current read-only review mode has no linked source attempt/candidate in its submit DTO. |
| Completed implementations cannot resume | An old native workspace may omit merged-main and coordinator changes; a repair must start from the delivered candidate. |
| Failed publication is terminal | There is no admitted append-only retry run. The last recovery used existing Git/GitHub providers manually and retained the failure record. |

Relevant modules: [worker](../../../deploy/mission-control/agent-runner/work_worker.py),
[store](../../../deploy/mission-control/agent-runner/work_store.py),
[worker DAO](../../../deploy/mission-control/agent-runner/work_worker_store.py),
[runner](../../../deploy/mission-control/agent-runner/work_executor.py),
[publisher](../../../deploy/mission-control/agent-runner/work_publication.py).

## Proposed flow and responsibility

An approved task can opt into bounded repairs within its original scope,
profile and included-quota policy. Each repair remains a separate leased
attempt. The original plan and every prior result stay immutable.

```mermaid
sequenceDiagram
    participant O as Owner / coordinator
    participant API as Admitted Work service
    participant DB as PostgreSQL DAO
    participant W as Coding worker
    participant G as Gemini CLI
    participant V as Independent verifier
    participant P as Trusted publisher
    O->>API: Approve plan and bounded repair policy
    API->>DB: Commit authorization and queued attempt
    DB-->>W: Notification; claim with lease
    W->>G: Approved plan and exact repair context
    G-->>W: Candidate, claimed fixes and check evidence
    W->>W: Stop scope and release writer
    W->>DB: Reconcile stopped artifact and queue review
    DB-->>V: Claim review of immutable candidate
    V->>V: Verify source, environment and fixed checks
    alt Reproducible source finding within repair policy
        V->>API: Bound feedback and repair disposition
        API->>DB: CAS record feedback and next attempt
        DB-->>W: Repair ready; repeat verification cycle
    else Infrastructure failure
        V->>DB: Record infrastructure outcome
        DB-->>O: Same-artifact retry or operator handoff
    else No progress, quota, changed scope or uncertain stop
        V->>DB: Record handoff; preserve source and receipts
        DB-->>O: Evidence and explicit takeover reason
    else Local gates and independent review pass
        V->>DB: Approve exact candidate for checked publication
        DB-->>P: Publication becomes claimable
        P->>P: Nonforce feature branch and draft PR
        P->>DB: Append remote receipt and check identities
        V->>V: Observe required hosted checks on exact PR head
        V->>DB: Record pass or new bound findings for repair
    end
    O->>API: Accept after required exact-head hosted checks pass
    API->>DB: Complete task; merge remains separate
```

These participants and review operations are a **proposal**, not new exported
APIs. PostgreSQL remains the sole state/lease authority. Notifications wake an
existing worker; they do not authorize execution. Reconnect scans repair missed
notifications without replaying a CLI or an uncertain remote write.

The agent receives a bounded feedback file and progress helper, never database
credentials. It may claim that a finding is fixed. Only independent verification
can close that finding or approve publication. Codex can remain the reviewer
without editing the agent's source during these rounds.

## Persisted feedback and execution contract

Introduce an immutable `ReviewFeedback` DTO and DAO record containing:

- Owner/project/task, expected revision, parent attempt and approved-plan identity.
- Parent result hash, trusted reconciliation digest, reviewed candidate commit
  and integration base. This distinguishes original native source from a merged
  verification candidate.
- Authorized publication target and expected remote head when updating an existing
  draft PR; the agent cannot select arbitrary repositories, branches or main.
- Gate-manifest digest, exact failed check/run, actual toolchain and environment.
- Stable finding IDs, category, reproduction, actual/expected behavior, evidence
  digest and acceptance checks. Supply bounded, redacted diagnostic excerpts;
  exclude credentials and unrelated private prompts. Log text is data, not instructions.
- Disposition, round, selected agent and the admitted repair-policy version.

A transaction records the feedback, binds it to the next attempt and emits the
event. Expected revision, source identity, receipt, authorization and idempotency
checks reject stale or duplicate decisions before any process starts. Worker
reads are lease-bound through a narrow DAO routine; arbitrary event notes are
not executable continuation authority.

The trusted verifier uses a scoped service identity for check observations.
An owner's admitted repair policy authorizes bounded correction decisions;
model-generated criticism alone grants no authority. Header, owner/project and
role checks precede DAO effects, and only the trusted review path can approve
publication or verification closure.

Pass `feedback_context` separately from `plan_context`. Mount its verified file
read-only and include its digest and finding IDs in the prompt. Preserve the
original plan note. A delivery regression must prove that a later feedback note
actually reaches the next runner invocation and that missing/stale feedback
prevents execution.

Keep all open findings in the ledger, but send a small causal batch per round
(initial proposal: up to three related findings). Require the agent to report
changed files, addressed IDs, commands/exits and remaining gaps. Reproducible
failing fixtures are more useful than a long instruction to "review everything."

For an unchanged same-task workspace, retain the existing receipt-validated
resume/session behavior. If verification added main changes or coordinator
patches, materialize a new repair workspace from that exact candidate. Preserve
the old receipt-bound workspace; do not overwrite it or repair an older base.
A completed parent needs a linked repair task. Start a fresh CLI session for
changed task/workspace/profile/binary scope unless continuation is independently
proven compatible; do not broaden warm-session trust merely to save tokens.

## Factory/provider boundaries

Extend the existing composition roots with narrow proposed ports:

| Port / owner | Responsibility |
| --- | --- |
| `ReviewContextProvider`, selected by `CodingWorkerFactory` | Acquire the immutable feedback authorized by the current attempt lease. |
| `VerificationProvider` | Execute trusted checks on the stopped candidate and return structured outcomes, versions, evidence and cleanup proof. |
| Pure `RepairPolicy` | Classify the outcome and select repair, retry infrastructure, revised plan, review-ready or handoff. No I/O or registry access. |
| Scoped review DAO | Persist findings, CAS decisions, attempts and append-only verification/publication runs. |
| Existing workspace/session providers | Own one writer, exact source materialization, compatible continuation and stop-before-transfer. |
| Existing Git/GitHub providers | Own credentials, nonforce publication and read-first reconciliation of uncertain acknowledgements. |

Bootstrap validates and freezes selected recipes, then creates and injects ports.
Workers never look up global registries. Keep these as cohesive private service
modules initially; a separate scheduler, service or crate is unnecessary.
The browser Rust controller owns actions/state; JavaScript renders and relays I/O.
It does not adjudicate findings, execute the repair policy or advance jobs.

Gate both normal publication queueing and missed-callback claim recovery on the
same exact-candidate review authorization. Use separate statuses for implementation,
verification, publication and final acceptance. A draft PR can exist while hosted
checks run; the task remains verifying until required checks finish. Terminal
verification/publication failures get an explicitly authorized new run, retaining
their old receipts rather than resetting history.

An authorized repair can update the same draft PR through the trusted publisher,
bound to its original publication identity and expected remote head. Reconcile
unknown write outcomes before retrying; never manufacture another PR merely
because an acknowledgement was lost.

## Repair and takeover policy

Start with an adjustable pilot maximum of three repair rounds per approved
scope. Keep existing process/storage limits and record the round's wall budget;
do not add a llama.cpp completion-token cap to coding-agent tasks.

| Outcome | Next action |
| --- | --- |
| Reproducible code/configuration failure | Give Gemini the bound feedback first; focused reproduction, then independent checks. |
| Toolchain-specific source failure | Give the exact compiler/version and diagnostic; verify under that target. An older local pass cannot close it. |
| Transient verifier/network failure | At most one same-artifact retry after cleanup proof; no model repair round. |
| Broken verifier, DB/auth/permission issue or unavailable required tooling | Operator repairs/provisions the boundary or records a blocker; agent must not weaken application checks to accommodate it. |
| New scope or permission requirement | Return to plan revision/owner decision. Existing repair authority does not expand. |
| Two successive rounds without verified progress, or round/time budget exhausted | Preserve source and produce a reasoned Codex handoff. |
| Quota/authentication stop | Preserve the checkpoint; no automatic account rotation or paid-credit fallback. Unknown headless allowance remains unknown; no consumer OAuth polling loops. |
| Cancellation, lease loss or uncertain stop | Stop admission, prove quiescence and reconcile before any successor writer. |

Progress means findings independently closed, not an agent's success statement
or a changing error string. Compare normalized check/path/error fingerprints
without ephemeral PIDs, timestamps or candidate SHA; bind source identity
separately. Newly introduced failures remain visible in the ledger.

Freeze the required gate policy outside the agent's writable source. Review
changes to Makefiles, CI, assertions or check scripts independently; legitimate
CI work may propose changes but cannot approve a weaker gate manifest. Reuse
green evidence only for verified identical artifacts and relevant environments.
New source changes rerun affected checks, and final required full gates remain.

## Smallest implementation order and proof

1. **Reliable feedback delivery.** Add the persisted DTO/decision, lease-bound
   read and separate runner feedback file. Prove latest-feedback delivery,
   stale-source rejection, replay/CAS behavior and unchanged original plan.
2. **Review-to-repair orchestration.** Add structured verifier outcomes and the
   pure repair policy through existing factories. Cover both publication
   admission paths, exact candidate materialization, linked completed-task
   repairs and append-only retry runs. No manual service hold should be needed.
3. **Pilot and visibility.** Show round, open/verified findings, last check,
   versions and handoff reason on Agents. Test an intentionally broken fixture
   that the selected agent repairs, repeated no-progress, lost events, duplicate
   delivery, cancellation, quota stop and infrastructure failure classification.

The first real pilot should be PR #1's Rust 1.100 `Infallible` diagnostic. Supply
the current head and failed hosted log, preserve provider wiring and strict lints,
and require the package profiles plus full hosted CI to pass. The coordinator
reviews the resulting diff and evidence before editing source itself. That pilot
has been prepared as feedback; it has not been submitted in this design review.

Measure original-agent verified closure rate, coordinator source-edit rate,
rounds per finding, repeated-failure rate, time to required green checks and
reported token/cache usage. Record each provider's real accounting gaps. After a
verified correction, preserve its regression and add the reusable lesson to the
relevant skill/contract. This is feedback and retrieval, not a claim of model
weight training.
