# External Muse intake proposal

Status: **dormant design and validation scaffold; no runtime integration**.
Muse email remains the primary manual transport. The separate
[native Muse Code adapter](muse-code-provider.md) defaults disabled. Neither
capability is enabled by this consolidation.

The proposal would bind untrusted external plans, progress and results to the
existing independently reviewed SDLC. Source retains payload validators, frozen
configuration DTOs, UTF-8 chunking and digest calculation. Factory, consumer and
store operations reject before acquiring database, process, account or worker
ports. Setting `enabled: true` is rejected; no work capability is advertised.

There is no installed consumer, background watcher, new database role, M20
migration, task-event writer or worker API grant. Existing coordinator/WorkerDB,
migrations1–19, exact receipt replay and P10 policy are unchanged. Proposed
persistence lives in the [storage contract](proposals/muse-intake-storage.md),
outside executable migrations.

## Rejected authority path

The incoming implementation fabricated a `systemd` receipt asserting
`scope_stopped: true` without observing a process scope. It passed a text-result
hash as a workspace reconciliation digest. Those hooks and its specific-task
claim API were removed completely. Validated text cannot prove execution or
source ownership, even when reviewed later.

Original claims also lacked owner/token fencing and expiry retry limits. Broad
INSERT rights admitted producer-controlled lifecycle fields. Progress events
were neither atomic with completion nor safe against duplicate replay. These
paths require a separate reviewed implementation rather than disabled flags.

## Retained data contracts

| Kind | Proposal data | Current behavior |
| --- | --- | --- |
| `submit_plan` | Project, base revision, bounded plan and distinct reviewer | Validation only |
| `publish_result` | Bounded UTF-8 chunks and awaiting-review outcome | Validation only; publication rejected |
| `progress` | Bounded nonempty summary | Validation only; no task event |

Digests are data, not execution or approval authority. No external model is
implicitly selected and no native P10 recipe is minted for these messages.

## Intended ownership and sequence

```mermaid
sequenceDiagram
    participant External as External Muse transport
    participant Intake as Future authenticated intake
    participant DB as Durable submission and outbox
    participant Admission as Trusted external receipt verifier
    participant SDLC as Existing independent SDLC
    External->>Intake: untrusted bounded envelope
    Intake->>DB: scoped idempotent submission
    DB-->>Admission: wake hint and authoritative durable row
    Admission->>Admission: verify identity, lease fence, revision and provenance
    alt current implementation
        Admission-->>External: unsupported; no task or worker mutation
    else separately implemented and reviewed contract
        Admission->>DB: atomic receipt and admitted event
        DB-->>SDLC: exact bound artifact for independent review
    end
```

Bootstrap would inject scoped transport/store ports through explicit factories.
The producer would have submission authority only, without native worker or
reconciler credentials. PostgreSQL remains durable authority; LISTEN/NOTIFY is
only a wake hint. JavaScript renders state and forwards actions.

## Requirements before activation

- Define a typed external-attempt receipt binding authenticated producer,
  correlation, owner/project/task revision, approved plan and exact source and
  content digests. Reject implementation/review without verified provenance.
- Restrict producer admission to permitted tasks and submission columns.
  Producers cannot set status, attempts, claim owner/token, expiry or results.
- Fence every claim/renew/release/finish with owner, random token, generation and
  unexpired lease; check compare-and-set results and bound crash-driven retries.
- Atomically commit the applied row, receipt, task transition and event. Repeated
  correlations replay the exact receipt; changed payloads conflict. Reject stale,
  cancelled, completed or unrelated tasks.
- Admit an externally supplied plan as a bound review artifact; submitting its
  text as another Plan prompt does not deliver that artifact or advance review.
- Preserve independent reviews, revision budgets, verification and trusted
  publication. External output cannot approve itself or merge.
- Test expiry races, crashes, replay, cross-project attempts, partial results and
  cancellation using isolated PostgreSQL and fake transports.

Current regressions prove dormant entrypoints acquire no ports, cannot publish
or reconcile, and install no intake authority. No real email or model calls run.
