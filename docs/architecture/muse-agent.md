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

## Existing read-only observation

The operator-managed `muse_events` account retains its authorized read access to
thirteen coding lifecycle tables, schema `USAGE` and database `CONNECT`. The
[read-access contract](../../deploy/mission-control/sql/README.md#optional-muse-read-access)
checks that exact capability without changing account credentials or granting
writes, function execution, task ownership or approval authority. The localhost
SCRAM HBA rule remains in place. The pre-existing default for future tables
created by `forge_agent_owner` in schema `forge_agent` also retains
non-grantable `SELECT` for this reader; global, other-owner/schema/type,
write, grant-option or extra-grantee defaults are rejected. The thirteen
current-table reads remain explicit. This observation capability is independent of
the unimplemented external intake below; it creates no consumer or watcher.

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
| `publish_result` | Bounded UTF-8 chunks, awaiting-review outcome and optional bounded summary | Validation only; publication rejected |
| `progress` | Bounded nonempty summary | Validation only; no task event |

Digests are data, not execution or approval authority. No external model is
implicitly selected and no native P10 recipe is minted for these messages.
The optional summary follows the existing4096-byte UTF-8 text limit. It remains
untrusted display data and cannot contain admission, lifecycle or receipt fields.

## Review of the proposed migration0020 revision

The later feature revision proposed typed SQL submission, random claim tokens,
three-attempt expiry handling and an atomic result-admission function. Its27
focused tests passed without skips against owned disposable PostgreSQL16. That
coverage did not establish the authority boundaries required for deployment.
Only the bounded optional result summary and its pure validation regressions
are retained from that revision. Its SQL, roles, service, coordinator hooks and
external completion path are not installed or merged into runtime source.

| Remaining gap | Required behavior before a separate implementation can be admitted |
| --- | --- |
| Producer scope | Derive allowed owners, projects and target tasks from authenticated grants; caller-supplied identifiers cannot grant authority. |
| Incomplete replay identity | Bind producer, correlation, owner, project, task, expected revision, kind and payload digest. A changed target must conflict even when the text matches. |
| Receipt replay before authorization | Authenticate and check target scope before returning an existing receipt; a known row UUID or arbitrary token cannot disclose another owner's receipt. |
| Incomplete lease fence | Every release, rejection, progress application and completion must check owner, claim identity, generation, token and unexpired lease, and must check the affected-row result. |
| Progress races | Lock and validate the current task revision/state and project binding in the same transaction as event sequence allocation and row completion. |
| External and native provenance mixed | Keep external artifacts separate from native P10 execution capability and workspace reconciliation. Text hashes and a renamed proof cannot establish implementation source or execution. |
| Plan not delivered | Opening a workflow with a stub prompt leaves the actual submitted plan unpublished. Bind the supplied plan itself to independent review without requiring a second unrelated result submission. |
| New SQL authority not fully verified | Exact owner, ACL, security-definer/search-path and function-body expectations must cover each new privileged operation and reject deliberate drift. |

Implementation and review artifacts also need exact approved-plan and source
identity checks. A green plan/revision text fixture cannot qualify a complete
implementation, independent review, CI or merge flow. The existing native
receipt contracts and migration1–19 verifiers are preserved unchanged.

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
  correlations replay the exact authorized receipt; changed identity, target,
  kind or payload conflicts. Check authorization before replay and reject stale,
  cancelled, completed or unrelated tasks.
- Admit an externally supplied plan as a bound review artifact; submitting its
  text as another Plan prompt does not deliver that artifact or advance review.
- Preserve independent reviews, revision budgets, verification and trusted
  publication. External output cannot approve itself or merge.
- Test expiry races, crashes, replay, cross-project attempts, partial results and
  cancellation using isolated PostgreSQL and fake transports.

Current regressions prove dormant entrypoints acquire no ports, cannot publish
or reconcile, and install no intake authority. No real email or model calls run.
