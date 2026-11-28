# Muse database provider

Forge supports an external Muse runner through PostgreSQL. Muse owns its runner
and compute; Mission Control does not launch its CLI or send email. Bootstrap
selects a scoped external provider through the normal WorkService factory and
injects its narrow DAO into WorkStore. The old [intake prototype](proposals/muse-intake-rejected.md)
remains dormant and is not this protocol.

The configured profile is `muse-db-primary`, backend `muse`, model metadata
`external-runner`, resource `forge-external-db-v2` for automatic workflows, with explicit
`execution_enabled: true`. The external policy digest is SHA-256 of that resource
identifier. It is separate from frozen native P10. Native `muse-primary` stays
disabled, and native workers cannot claim external recipes. Existing AGY/Codex
project workflows retain their configured roles and immutable policy identities.

## Admission, dispatch and artifacts

A task submitted through the existing portal/API creates its ordinary durable
`coding_tasks` row, queued event and external assignment in one transaction.
Idempotent receipt lookup happens before recipe resolution or enqueue, including
when a profile is subsequently disabled. Changed intent conflicts. Scope grants
bind the authenticated database principal, service, owner, project and profile;
caller-supplied names do not grant authority.

The task picker exposes an external profile only after a fresh scoped heartbeat.
It emits `transport: external_db`; its display model does not assert a discovered
model version or account allowance. The existing trusted project revision is
used. An installation with no known project revision cannot invent one merely
because the external runner advertised. Account usage configuration stays
separate: this provider does not request native Muse quota or model metadata.

External Plan and Review modes return text artifacts. Their detail projection is
`external_result`, containing `assignment_id`, `provider: muse-db`, `sha256`,
`content`, `submitted_at` and `summary`. Content is at most 256KiB and the summary
at most 4096 UTF-8 bytes. These are report-transport bounds, not generation-token
limits. No native attempt, systemd proof, workspace digest or reconciliation
receipt is manufactured.

The raw transport status `awaiting_review` means an actual artifact has arrived.
For a configured M22 project, the separate external workflow automatically
dispatches independent review and continues through implementation and trusted
delivery below. A standalone v1 report retains its manual transport semantics.
A Muse Review artifact cannot approve another task or merge code. Native attempts
and external assignments remain distinct sources of evidence.

```mermaid
sequenceDiagram
    participant Portal as Portal / Rust
    participant Service as WorkService + injected external port
    participant DB as PostgreSQL
    participant Muse as External Muse runner
    participant Coordinator as Owned workflow coordinator
    Portal->>Service: submit Plan or Review / exact intent
    Service->>DB: transaction: task + queued event + assignment
    DB-->>Muse: LISTEN wake hint
    Muse->>DB: advertise, scoped claim
    DB-->>Muse: intent, generation, token, lease
    Muse->>DB: renew / progress / bounded finish
    DB->>DB: artifact + receipt + awaiting_review event atomically
    Coordinator->>DB: bounded lease/deadline/cancellation reconciliation
    Portal->>Service: get task
    Service->>DB: scoped external artifact read
    Service-->>Portal: external_result / awaiting_review
```

## Runner SQL interface

Connect using the operator-provisioned account. Do not embed credentials in
prompts, task payloads, logs or the repository. The existing thirteen read grants,
owner/schema-scoped future-table SELECT default, database connectivity and
localhost SCRAM rule remain intact. New external operations use only the
reviewed function allowlist; private assignment/artifact tables are not directly
writable by the runner.

```sql
SELECT forge_external_agent_api.capabilities();
SELECT forge_external_agent_api.advertise('homelab', 'muse-db-primary', 'my-runner');
LISTEN forge_external_agent_jobs;
SELECT forge_external_agent_api.claim('homelab', 'muse-db-primary') - 'external_context';
```

Subscribe before scanning and rescan after reconnecting. Notifications are wake
hints; durable rows determine whether work exists. Advertise regularly before
the 90-second freshness window expires. A null claim means no eligible work.
The runner binds the returned assignment ID, generation and secret token as
parameters to subsequent calls; it must not log the token.

```sql
SELECT forge_external_agent_api.renew($1::uuid, $2::integer, $3::uuid);
SELECT forge_external_agent_api.progress($1::uuid, $2::integer, $3::uuid, $4::text);
SELECT forge_external_agent_api.finish($1::uuid, $2::integer, $3::uuid, $4::text, $5::text);
SELECT forge_external_agent_api.release($1::uuid, $2::integer, $3::uuid);
SELECT forge_external_agent_api.artifact($1::uuid);
```

A claim carries a bounded five-minute lease and an overall 24-hour deadline.
Renewal cannot extend the deadline. Up to three generations are allowed;
expired claims return to queued or fail when the budget/deadline is exhausted.
The existing owned coordinator runs scoped database maintenance at most once
per 30 seconds even when no portal page is open. No extra Muse process, account
poller or email watcher runs on Mission Control.

Finish binds the exact scope, generation, token, content digest and summary.
Retry the same finish only to reconcile a lost acknowledgement; changed content
or stale authority fails. Progress and completion events commit with their row
changes. Progress summaries are database-only in this slice; the portal shows
task status and the final external artifact, without a native progress or attempt
projection. Cancellation fences further mutations. The external runner owns
stopping its computation; a database receipt does not claim that Mission Control
observed or killed an external process.

## Construction and verification

WorkServiceFactory selects ExternalWorkProviderFactory only for configured
external profiles. Factory construction is effect-free. WorkStore owns each
connection/transaction cursor and supplies that cursor to the external port;
workers do not look up registries. The existing coordinator owns maintenance
lifetime and quiesces through its normal close path before scoped ports close.

Tests use substituted factories, fake ports and owned socket-only PostgreSQL.
They cover admission rollback, duplicate/disabled-profile replay, scoped
readiness, native-claim exclusion, real external artifacts without native
receipts, escaped response-size bounds and lease recovery. No real Muse/model,
email, account or paid API request is needed to verify this transport.

## Automatic external SDLC (M22)

Configured projects now use `forge-external-db-v2` for new Muse Plan and
Implement assignments. The original v1 intent and recipe remain frozen. An
existing submitted v1 Plan is adopted by its real task/assignment/artifact
identities; it is never rewritten into a native execution attempt.

The typed `external_workflows` project configuration selects distinct external
Muse author and native Codex reviewer profiles, freezes all three recipes, and
records three plan revisions, three implementation corrections and automatic
merge delegation. The API coordinator performs database work only. A separate
reconciler-owned delivery worker holds the Git/Jenkins ports. Native Muse CLI
execution and email remain disabled.

A submitted Plan automatically queues an independent native read-only reviewer.
The complete immutable artifact, original request and acceptance criteria are
staged as `external-review.json`, outside the small task prompt. Code reviewers
also receive the approved Plan and exact clean candidate/tree/diff identity.
They return a structured verdict bound to the review, source assignment, artifact
hash and context hash. Their native attempt must actually stop, publish all
chunks and reconcile before a verdict can move the workflow. Invalid/oversized
JSON receives one same-source reviewer correction; a second invalid response is
a recorded human exception. Ordinary revision verdicts return full feedback to
Muse, with a maximum of three author revisions per stage.

The claim query above omits the duplicate parsed context. The runner verifies the
SHA-256 of the exact UTF-8 `external_context_json` against `external_context_sha256`
before parsing it once. Pass the assignment and its verified working context to the
model. Keep lease tokens, renewals and receipt metadata in the runner.

After Plan approval, Muse implements on its assigned branch and returns the
following manifest through the bounded `finish` operation.

```json
{"version":1,"candidate_commit":"<40 lowercase hex>","candidate_tree":"<40 lowercase hex>","base_revision":"<supplied trusted base>","summary":"Implementation and verification report"}
```

The delivery worker fetches and sanitizes the candidate into a clean Forge
commit, preserving its exact tree while excluding untrusted author Git history.
Independent Codex review must attest every protected gate change. Trusted Jenkins
checks and merge receipts must match that same candidate/tree/gate manifest.
Changed main triggers bounded source refresh, rebase, renewed independent review
and CI. Only a trusted exact-source merge receipt completes the root workflow.
GitHub Actions remain disabled.

The portal projects one root envelope with linked child reviews, phase,
revision counts, candidate and merge commits. Every server transition appends a
root checkpoint with the same correlation ID; these are server workflow events,
not invented Muse progress or native stop/review evidence.

Cancellation stops admission and requests native child cancellation without
claiming the process has stopped. A durable `cancel` delivery action reconciles
and stops an owned Jenkins queue/build even after author access is revoked or
the workflow becomes terminal. It retains the original action ID and last real
receipt; cleanup still requires the existing scoped delivery grant. Once the
merge intent is committed, cancellation is disabled until that exact remote
write is reconciled.

The [external workflow sequence and operator contract](external-agent-workflow.md)
describes the complete boundary and bootstrap configuration.
