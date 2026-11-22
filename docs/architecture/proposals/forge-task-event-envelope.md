# Task correlation IDs and the event envelope

Status: implemented in M21 and the ABI12 task reader; deployment remains separately verified.

The source includes required UUID projections, a separate50-event history page,
a 256KiB summary budget, 64 bounded cursor bookmarks, explicit First/Previous/Next
controls, keyed event disclosures and the unchanged 21-entry native milestone
window. M22 adds the separately scoped [external review/delivery workflow](../external-agent-workflow.md).
SQL/DAO, Rust protocol/model, host and actual owned browser checks cover these
paths. Publishing this source does not substitute for live migration/rollout receipts.

Derived from Muse's immutable Plan artifact for task
`7699599c-b5d0-4118-9bb0-3f8f917c5843`, assignment
`8d2398ad-8841-4bc6-892e-12d4c89bbb75`, submitted 21 November 2026 at 05:09 UTC.
Original artifact SHA-256:
`9bb7fc6d7b937f6fd4213fd09644e2a2e81af4e1ce93823519616712033455aa`.
That digest identifies Muse's original content, before the review corrections
incorporated here. The original remains unchanged in PostgreSQL.

Review retained Muse's task-envelope, durable trace, derived event correlation
and typed Rust presentation design. It corrected request strictness, bounded
history paging, external-progress projection, compatibility and migration
requirements against source checkpoint `2a0bcbc`.

The original plan remains an immutable review artifact. The implemented M22
dispatcher creates a real native Codex review task with the complete bound context;
this document itself grants no review approval or execution authority.
Acceptance criteria: the portal shows a single task, and clicking on the task
displays that task's events.

## 1. Goal

Every coding task gets a durable correlation ID at submission. Every event the
task produces gets its own event ID and carries the task's correlation ID. The
Agents section lists one row per task — the task is the envelope — and the task
reader shows the task's full event history in sequence order: what was done,
when, and in what state the task moved through.

This is a tracing and presentation change. It does not change task admission,
workflow policy, recipes, attempts, receipts, or publication. Stored event kinds, payloads and lifecycle transitions remain unchanged.
Typed response projections expand as specified below.

## 2. Original reviewed baseline

The table preserves the design audit at main `2a0bcbc`; the gaps below are now
implemented by M21 and the ABI12 protocol/model/generator changes.

| Piece | As built | Gap |
| --- | --- | --- |
| `coding_tasks` (`deploy/mission-control/sql/migrations/0009_coding_admission.sql:1`) | `id` UUID primary key, `idempotency_key`, `intent_digest`, `base_revision`, `mode`, `agent_profile` | The durable task UUID and scoped event foreign keys already identify the task throughout its lifecycle. The proposal adds a separate operator trace handle; the idempotency key remains a client replay handle. |
| `coding_events` (`0009_coding_admission.sql:105`) | Key is `(owner_id, project_alias, task_id, sequence)`; `event_kind`, `payload` | No event ID and no correlation ID. An event is addressable only as "sequence N of task T". |
| Event writers | 6 Python INSERT sites in `work_store.py` (lines 459, 861, 996, 1162, 1312, 1420), 2 in `work_workflow.py` (628, 802), plus SQL-function INSERTs in migrations 0010–0017 and `forge_external_agent._event` (0020) | Writers are scattered across Python and SQL. Any design that requires every writer to pass new columns is fragile and will drift. |
| Runner API envelope (`work_api.py:517` `decode`) | Per-request `correlation`: the body `request_id` must equal it | The request identifier belongs to one API exchange. Durable task/event identities already exist separately; this proposal adds explicit correlation metadata to their projections. |
| Store events read (`work_store.py:625` `events`) | Returns `{sequence, kind, created_at, progress?}`, paginated by `after_sequence`, limit 1–100 (default 50); `progress` attached only for `checkpoint` events | No event ID, no correlation in the projection. |
| Rust protocol (`src/rust/forge-ide-ui/src/protocol/work.rs`) | `WorkTaskSummary` (:552), `WorkTaskDetail` (:624), `WorkLogEvent` `{sequence, kind, created_at, progress?}` (:739); all `serde(deny_unknown_fields)` with `is_valid` validators | No correlation on summary or detail; no event ID or correlation on log events. |
| Agents list (`src/rust/forge-ide-ui/src/model/agents.rs`) | `tasks: Vec<WorkTaskSummary>` — one row per task | Already envelope-shaped. Nothing to change except surfacing the correlation. |
| Agents reader (`agents.rs:317` `keep_progress`) | Retains only `checkpoint` events whose progress matches the current attempt, capped at 21 entries (plan + latest 20); comment: "history stays in PostgreSQL" | The reader shows a checkpoint-only window, not the event history. Lifecycle events (`queued`, `started`, `resumed`, `awaiting_review`, `failed`, `cancelled`, …) are dropped, and anything older than the window is invisible. |

## 3. Concepts

| Name | Definition | Rules |
| --- | --- | --- |
| `correlation_id` | A UUID assigned to a task at submission. The task's trace handle across systems. | Generated server-side at submit (`gen_random_uuid()`). Immutable after insert. Unique per task. The submit request has no correlation field. The existing strict request decoder rejects caller-supplied extra fields with `invalid_request`; it does not silently ignore them. Trusted database writers use the server default. |
| `event_id` | A UUID assigned to an event at insert. The event's stable identity. | Generated at insert (`gen_random_uuid()` default). Unique across all events. The `(task_id, sequence)` key stays the ordering key; `event_id` is the identity key. |
| Event correlation | Each event row carries its task's `correlation_id`. | Derived, never caller-supplied: a trigger copies it from the task row at insert (section 4). An event insert that names a correlation gets the task's value regardless. |

What these are not:

- The correlation ID is not the task ID. The task ID is the internal primary
  key; the correlation is the handle shown to operators and carried into logs,
  mail, and external systems. The task ID remains the internal entity key. A unique, immutable per-task
  correlation cannot later become a shared workflow key. Future grouping needs
  a separate workflow/root correlation and, where needed, a causation ID.
- The correlation ID is not the idempotency key. The idempotency key is a
  client-chosen replay handle scoped to one submit call. The correlation is
  server-generated and lives for the life of the task.
- The correlation ID is not the request correlation in `decode`. That value
  lives for one API call and is never stored. The task correlation is durable.

## 4. Database — migration 0021

Migration0021 is the implemented task-event trace migration. Migrations 0001–0020
remain immutable. No existing column changes type and no row is deleted.
Backfill and DDL still require the rollout rehearsal in section 9.

| Change | Definition |
| --- | --- |
| `coding_tasks.correlation_id` | `uuid NOT NULL DEFAULT gen_random_uuid()`, `UNIQUE`. Existing rows get distinct UUIDs from the default at `ADD COLUMN` time (PostgreSQL evaluates a volatile default per row). |
| `coding_tasks` immutability | A `BEFORE UPDATE` trigger rejects any change to `correlation_id` (same pattern as the existing immutable-binding triggers in 0020). |
| `coding_events.event_id` | `uuid NOT NULL DEFAULT gen_random_uuid()`, `UNIQUE`. Existing rows get distinct UUIDs from the default. |
| `coding_events.correlation_id` | `uuid NOT NULL`. Backfilled in the migration from the owning task (`UPDATE … FROM coding_tasks`), then the column is set `NOT NULL`. |
| Event correlation trigger | `BEFORE INSERT` on `coding_events`: `NEW.correlation_id := (SELECT correlation_id FROM coding_tasks WHERE (owner_id, project_alias, id) = (NEW.owner_id, NEW.project_alias, NEW.task_id))`. No lock is needed: the task correlation is immutable, so the value cannot change under the trigger. Reject a missing owner/project/task match explicitly. A null derived correlation can otherwise fail `NOT NULL` before the existing foreign key is checked; do not promise one specific FK diagnostic. |
| Event identity immutability | A `BEFORE UPDATE` trigger rejects changes to `event_id`, `correlation_id` and the owning `(owner_id, project_alias, task_id, sequence)` identity. Install it after the intentional backfill. |
| Index | `(correlation_id, sequence)` on `coding_events`, for the trace query ("all events for this correlation, in order"). The existing `(task_id, sequence)` key stays the primary ordering path. |
| Verifiers | Update current version-aware exact snapshots in `verify.sql` and `verify-feedback.sql`, plus every affected transitive contract. Pin columns/defaults/indexes and trigger body, owner, execution security, safe search path, ACL and enablement. Preserve native/external grants and Muse's existing reads/defaults. New internal functions must not acquire public execution. A drifted definition fails verification. |

Why a trigger instead of writer changes: there are 8 Python INSERT sites and
SQL-function writers across 9 migrations, including the reviewed external
transport (`forge_external_agent._event`). Defaults plus one trigger cover all
of them with zero writer edits, and the derivation rule means a writer cannot
mis-attribute an event to another task's correlation. Future writers get the
behavior for free; the rule for new writers is "insert the event, do not set
the correlation."

New trace columns are metadata. Preserve the byte identities of historical
recipes, receipts and assignment intent hashes. In particular, keep M20's fixed
`_intent` fields unchanged; introducing trace keys into that frozen intent would
invalidate existing external assignments. Audit any complete-row JSON/hash
consumers during implementation and test existing assignments across upgrade.
A later cross-system envelope must version its metadata contract explicitly.

## 5. API and store

Wire through the normal WorkService factory path (`wire-forge-providers` skill);
no new service or port. The store owns the cursors, as today.

| Projection | Change |
| --- | --- |
| `WorkTaskSummary` | Add `correlation_id: String`. Required, validated as a UUID (`valid_chat_id`). |
| `WorkTaskDetail` | Add `correlation_id: String`, same validation. No events list on the detail: events stay behind the paginated events read. |
| `WorkLogEvent` | Add `event_id: String` and `correlation_id: String`, both required UUIDs. |
| Store `events()` | Select `event_id, correlation_id` alongside the existing columns and return them per item. Keep task-scoped `after_sequence` pagination. Preserve validated native `progress` and add a separately typed external progress projection. |
| External progress | Validate the M20 `{provider, assignment_id, generation, summary}` checkpoint against the selected task's real assignment and the event's claimed generation. Expose a bounded typed external summary with no native `attempt_id` or process proof. Reject mismatched/untrusted summaries; do not forward arbitrary payload JSON. |
| Store `list_tasks()` / `get()` | Select and return `correlation_id`. |

The events endpoint stays task-scoped in this slice: the portal already knows
the task ID when it opens the reader, so no new lookup path is needed. A
correlation-scoped task lookup (resolve a correlation to its task) is a
follow-on, listed in section 8.

`serde(deny_unknown_fields)` and the `is_valid` validators extend to the new
fields in the same style as the existing ones. Validate new UUIDs at the typed boundary. Match every event's correlation to the
selected task as well as checking its task/project/request context. Event kinds
remain a closed typed enum; adding a future kind requires protocol qualification.

The previous atomic UI ABI was11. The implemented atomic bundle is ABI12
for the required response fields: host validation, Rust/WASM, generated glue,
manifest and fixtures must agree. Do not send wider responses to an old strict
consumer or weaken `deny_unknown_fields` for compatibility.

## 6. Portal — envelope list and full event history

Authoring follows `design-forge-ui` (typed Rust/YAML declarations, generated
assets, no hand-written HTML/CSS/JS) and `design-modern-ui` (hierarchy before
authoring; telemetry/logs/timestamps rules). WASM owns the domain state and the
event list; the main thread owns presentation only.

### 6.1 List: the task is the envelope

The list already renders one row per `WorkTaskSummary`. It stays that way and
never renders event rows. Keep the full correlation in the reader and an
accessible copy/detail control. Detail-only is the default here, preserving the
mobile row's status/profile/time hierarchy. A short first-eight-character label
can be added to list rows only if phone layout and interaction checks justify it;
it never replaces the full identity or serves as a unique lookup key.

### 6.2 Reader: the full event history

Add a separate timeline instead of turning `keep_progress` into an unbounded log:

- Present lifecycle metadata for every currently typed event kind in sequence
  order, including queued, started, checkpoint, resumed, awaiting_review, failed
  and cancelled. Every stored event remains reachable through pagination.
- Preserve the existing current-attempt milestone panel, its validated progress
  filter and its 21-entry bound. Historical timeline metadata may cover earlier
  attempts, but a checkpoint summary must be verified as belonging to that task's
  actual attempt before display. Foreign or mismatched progress remains excluded.
- Introduce explicit Previous/Next/First timeline controls over pages of 50 events.
  The current model automatically drains event pages; it has no user-facing
  history Load more control. Keep that existing bounded milestone path separate
  from the new timeline requests and stop automatic timeline page draining.
- Retain one timeline page, at most 50 events, with a 256KiB total UTF-8 summary
  budget. Bound cursor bookmarks (for example, 64 entries); First can reset them
  and reach older history again. Evict cached pages rather than accumulating the
  entire database log. Account for JSON escaping and prepared-view size within
  the existing message limits, and never increase those limits to hide overflow.
- Each row shows a semantic state pill and a relative or short UTC date. Exact
  time, full event ID and provenance are available in keyboard/touch-accessible
  details. Show the full task correlation once in the section header.
- Key disclosures by stable event ID and preserve open disclosure/focus while
  the same page refreshes. Keep selection, page navigation and stale-result
  acceptance in Rust; the main thread renders the accepted projection.
- External checkpoints use the separately validated external-summary DTO in
  section 5. M20's summary is currently discarded by the native-only projector,
  so this requires an explicit projection change. Never reuse an assignment ID
  as a native attempt ID. The result stays in `external_result`; the timeline
  identifies its arrival without copying the artifact into event payloads.

### 6.3 Interaction trace

Click task row → WASM `WorkGet` + task-scoped `WorkEvents` (wider
typed results, separate bounded timeline state) → worker glue → host/agent gate → store reads → typed results
accepted by WASM (stale replies ignored by the existing revision guards) →
reader projection renders the envelope header and the history list. History page controls are new typed local actions using the existing admitted
WorkEvents transport. No new backend route or main-thread fetch is needed.

## 7. What this slice does not do

- No workflow-level correlation. A workflow's plan task, implement task, and
  review task each keep their own correlation. Grouping tasks under one
  workflow correlation is a separate design.
- Mail and usage correlation remain separate work. M22 carries the root task
  correlation into trusted Jenkins delivery/check receipts; it does not turn that
  per-task ID into a shared correlation for all child tasks.
- No change to stored event kinds, payloads, lifecycle transitions, or retention.
  Response projections gain validated identity and external-summary fields.
- No change to the external transport functions: the trigger covers
  `forge_external_agent._event` without touching the reviewed 0020 code.
- No new background process, poller, or watcher.
- Automatic external Plan review, implementation dispatch and the Jenkins bridge
  are implemented separately in M22; they are not authority conferred by trace metadata.

## 8. Verification

| Layer | Check |
| --- | --- |
| SQL contract | Disposable PostgreSQL: submit a task, assert a correlation is assigned and immutable (update rejected); emit events through the Python store, the workflow coordinator, and the external transport, and assert every event carries its task's correlation and a distinct event ID; assert an insert naming a foreign correlation still lands with the task's value; assert `verify.sql` pins pass. |
| Rust protocol | `WorkTaskSummary` / `WorkTaskDetail` / `WorkLogEvent` accept the new fields and reject malformed UUIDs and unknown fields; model tests cover two pages of a 60-event task, all typed lifecycle kinds, bounded resident history, bookmark bounds and stale-page/correlation rejection. Preserve the native milestone cap and foreign-attempt rejection, and verify scoped external summaries separately. |
| Browser (`test-forge-ui`) | Journey: submit a task, open it from the list, assert the reader shows the envelope header with the full correlation and the history rows in sequence order with kind pills and dates; exercise explicit history paging, disclosure/focus preservation, malformed or mismatched progress rejection and escaped summaries; assert DOM content, not just element existence; capture screenshots at phone and desktop widths while the history is visible. Existing `make ui-qa` gate stays green. |
| Regression | Full agent-runner suite and `cargo test` green; no writer behavior changes (the trigger is the only new write path, covered by the contract tests). |

## 9. Rollout

Rehearse the migration and exact verifiers on a disposable restored backup before
production. Record row counts, rewrite/storage needs, lock duration and chosen
backfill strategy. PostgreSQL16 `ALTER TABLE` can acquire `ACCESS EXCLUSIVE`,
and adding a volatile default can rewrite existing rows and indexes. This plan
therefore makes no zero-downtime claim. Use explicit lock/statement timeouts and
a coordinated maintenance window; choose a phased backfill if the measured data
size requires it. See [PostgreSQL16 ALTER TABLE](https://www.postgresql.org/docs/16/sql-altertable.html).

Take and restore-verify a fresh backup, preserve existing permissions and frozen
receipt/intent identities, and coordinate schema21, API, host and the qualified
ABI12 asset bundle. New code requires the new columns, while old strict consumers
reject the wider DTOs. Do not deploy these independently or use silent fallback
fields to disguise a mixed release. Rollback must account for the installed
schema and preserve issued trace identities.

After deployment, spot-check an old task and its backfilled identities, then a
new task and its generated identities. Verify native and external event writers,
historical/active assignment compatibility, scoped access, bounded paging and
phone/desktop rendering before reporting implementation complete.

## 10. Recorded choices and follow-ons

1. Show the full task correlation in the reader. Keep list rows compact by
   default; a short optional list label needs layout evidence.
2. Backfill fresh UUIDs for existing tasks and events once. Preserve them on
   replay, reinspection and subsequent upgrades.
3. A correlation-scoped lookup is a separate admitted API change. Continue
   authorizing by owner/project/task; a correlation is an identifier, not access.
4. Shared workflow correlation and causation IDs are separate future fields.
   The immutable, unique task correlation keeps its current per-task meaning.
5. M22 now implements the separately authorized artifact-to-review binding, full
   native reviewer context and verified verdict handling. Trace metadata alone
   still cannot approve an artifact or alter its review state.
