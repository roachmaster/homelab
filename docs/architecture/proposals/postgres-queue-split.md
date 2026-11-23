# Postgres for state, a queue for delivery: splitting the Forge event stream

**Status: proposal, drafted 2026-11-22 by Muse at Leonardo's request. Not
implemented. The proposal introduced this document only, with no broker,
queue-consumer or orchestration code. Other repository changes do not activate
this design.**

## 1. Goal

Postgres owns durable state: task and subtask lifecycle status, ownership, and
the audit log. A durable message queue owns delivery. An orchestration
microservice owns monitoring and reconciliation, replacing the agent-side
polling hooks. The SDLC in section 5 ships the split in four phases, each
gated and reversible.

The determinism claim this design exists to make true: **the database status
is the single source of truth for what a record's state is; the queue is only
a hint that something may need attention.** A record in progress cannot be
processed a second time. A crash anywhere — queue down, app down mid-process,
relay down mid-publish — converges on the same outcome, because every restart
re-derives pending work from database status, not from queue position. Queue
position is ephemeral; database status is durable.

## 2. Why now

The event stream is supposed to ensure ACID, but states have caused issues in
practice. The evidence, all observed on the live system:

- **The midnight sweep, 2026-11-22 00:00 CST.** Seven open tasks were
  cancelled in one sweep. The audit log recorded what happened but not who or
  why — no actor is carried in the event payloads, so the cause was never
  determinable from the database.
- **No replay on reconnect.** `work_worker.py:408` logs it plainly: "Coding
  worker reconnecting; no assignment is automatically replayed". Recovery of
  an in-flight assignment depends on lease expiry plus `reconcile_pending()`,
  not on the delivery mechanism.
- **The external side has no worker loop at all.** Muse's readiness depends on
  a 60-second advertise hook running on a VM that gets replaced; on
  2026-11-21 the profile went stale seven times in about five and a half
  hours. A missed heartbeat is silent until something polls for it.
- **The watcher is a hand-rolled queue consumer.** The events watcher polls
  `coding_events` every 60 seconds with a watermark stored in a file on the
  agent VM. Polling latency, a file-based consumer offset, and no replay are
  exactly the problems a real queue solves.

NOTIFY's actual role today makes the split less radical than it sounds: every
`pg_notify` call in the tree sends an **empty payload**. NOTIFY is a wake-up
ping, not a data carrier — the native worker already re-reads the database on
every iteration (`work_worker.py`, `run_forever`, 5-second scan). Delivery is
already database polling with a wake-up optimization. This design makes the
architecture honest about that, and gives the wake-up durability, replay, and
real consumer offsets.

## 3. Current baseline

Traced against main `c738faa`.

| Piece | As built | Gap |
| --- | --- | --- |
| Lifecycle status (`coding_tasks`, `coding_workflows`) | `status`, `revision`, lease columns; claim is conditional (`SELECT ... FOR UPDATE`, status check, transition to in-progress plus lease) | The idempotency guard exists for claims but is not the stated rule for every consumer of every wake-up |
| `coding_events` | Audit log with `event_id` and `correlation_id` (0021), per-task `sequence`; writers in `work_store.py` (6 sites), `work_workflow.py` (2), `work_external_workflow.py` (4), plus SQL-function inserts | No retention job (unbounded growth); writers scattered, so any new per-event requirement drifts unless audited |
| NOTIFY channels | `forge_coding_jobs` (dispatch wake-up), `forge_external_delivery`, `forge_agent_jobs`, `forge_agent_events`, `forge_jenkins_events`, `forge_coding_usage_refresh`; all empty-payload pings | Best-effort: a notification sent while a listener is disconnected is lost; no replay, no consumer offsets, no backpressure |
| LISTEN sites | `work_workflow.py:143` (`WorkflowStore.listen`, `wait` with 1-second select slices), `queue_store.py:101`, `jenkins_store.py:136`, `forge_gpu_worker.py:58` | Each listener re-implements wait/reconnect; the loss window is real for any pure NOTIFY consumer |
| Native worker loop | `work_worker.py:run_forever`: LISTEN plus a 5-second scan; `run_once` does `reconcile_pending()` then `advertise()` then `claim()` then `process(lease)` | The 5-second scan is the actual delivery mechanism and the only thing that saves a missed NOTIFY; it is implicit, not a designed guarantee |
| External transport | 0020/0022 Postgres functions (`advertise`, `claim`, `renew`, `finish`, `release`); readiness is a 90-second freshness window | No worker loop on the agent side; readiness freshness is monitored from outside the repo |
| Lease/dead-letter prior art | `jenkins_store.py:543` — exhausted expired leases transition cleanly to `dead_lettered` instead of looping | The pattern exists for Jenkins deliveries; the coding path relies on `reconcile_pending()` with no equivalent dead-letter state |
| Monitoring today | Agent-side hooks: `forge-heartbeat` (60-second advertise), `forge-events-watch` (60-second poll), 30-minute heartbeat-tick checks over SSH | Lives outside the repo, costs agent tokens on every tick, and stops when the agent VM is replaced |

Atomicity note, verified in code: `_insert_task`
(`work_external_workflow.py:457`) inserts the task row, the external-workflow
row, and the `queued` event on the same cursor, in one transaction, and the
`pg_notify` fires in that same transaction (Postgres delivers NOTIFY only on
commit). The state change and its event already commit atomically at this
writer. Section 4.1 requires auditing the remaining writers to the same bar —
the split depends on it, and the audit is a phase-1 task, not an assumption.

## 4. Design

### 4.1 The split: outbox, relay, queue

- **Every state transition writes its event in the same transaction.** This is
  the one rule the whole split rests on. `coding_events` **is** the outbox —
  no second table, one audit trail, one write. A dedicated outbox table would
  duplicate the write and split the audit trail for no gain.
- **A relay service publishes the outbox to the queue.** The relay reads new
  events in sequence order and publishes them to NATS JetStream. It stores its
  position durably in Postgres (a small `relay_progress` table), so a relay
  crash resumes without loss. Delivery is at-least-once; `event_id` is the
  dedup key on the consumer side.
- **NATS JetStream is the queue.** Single binary, runs under systemd like
  everything else on Mission Control, durable streams with replay and consumer
  groups. No Redis exists in the tree, so there is no incumbent to displace;
  Kafka is overkill at this scale. One stream with subjects
  (`forge.coding.jobs`, `forge.external.delivery`, `forge.jenkins.events`,
  `forge.coding.usage.refresh`) keeps operations to a single stream; one
  stream per channel family is the fallback if subject-level retention ever
  needs to differ.
- **Consumers subscribe for wake-ups and keep the database scan.** The native
  worker's 5-second scan stays as the delivery fallback. The queue makes
  wake-ups durable and replayable; the scan makes delivery robust to the queue
  being down entirely. Neither mechanism alone is the guarantee; the pair is.
- **NOTIFY is retired last.** It stays as a secondary wake-up through phase 3
  and is removed only in phase 4, after the queue has proven itself.

### 4.2 Status-based idempotency: the safety net

This is the core of Leonardo's requirement, stated as a rule every consumer
must follow:

> **A queue message never causes work by itself.** The consumer claims the
> database record in a transaction: `SELECT ... FOR UPDATE`, require the
> expected predecessor status, transition to in-progress plus a lease. If the
> status is anything else, the message is a duplicate or stale — acknowledge
> it and drop it.

Crash cases, each converging on the same outcome:

| Failure | What happens | Why no duplicate |
| --- | --- | --- |
| Queue down | Messages wait durably; consumers fall back to the database scan | The scan claims through the same status transition |
| App down mid-process | The record stays in-progress with a lease | The lease blocks any other claim; on expiry the orchestrator returns the record to a retryable state |
| Relay down mid-publish | Relay resumes from its stored position and republishes | `event_id` dedup drops the republish |
| Worker crashes after processing, before acknowledging | The message is redelivered | The status check sees the record already past the predecessor state and drops the redelivery |

**Actor recording closes the midnight-sweep gap.** Every lifecycle event
payload carries an `actor` — the service identity, the operator, or the agent
profile that caused the transition. A future sweep is attributable from the
audit log alone.

### 4.3 Orchestration microservice: `forge-orchestrator`

A new systemd service on Mission Control. Python, in
`deploy/mission-control/agent-runner/`, because it shares the store/DAO code
and the lease logic with `work_worker.py` — a Rust service would reimplement
both. Configuration under `/etc/homelab-forge/orchestrator/`, following the
existing service layout.

Responsibilities:

1. **Queue health.** Stream depth, consumer lag, relay lag (outbox position
   versus published position), dead-letter counts.
2. **Stuck-record detection.** Tasks in-progress past their lease, queued past
   their deadline, assignments leased past `lease_until` — the check the
   heartbeat tick currently performs over SSH moves into the repo.
3. **Reconciliation.** Expire dead leases and return stuck work to a retryable
   state **through the existing `reconcile_pending()` path**. The orchestrator
   calls the reconciler; it does not reimplement it. What it cannot fix, it
   escalates with the evidence attached.
4. **Readiness monitoring.** Alert when an external profile goes stale.
   Monitor-only in v1: it does **not** advertise on any agent's behalf — the
   readiness signal stays the agent's own, or it stops meaning anything.
5. **Health record.** Writes health state to a Postgres table the portal reads,
   so the Agents section shows queue and fleet health next to the tasks.

What it does **not** do: no LLM calls, no task-content judgment, no agent
decisions. Deterministic rules only. That is the token saving — it replaces
`forge-events-watch`, the 30-minute heartbeat-tick checks, and the
stuck-assignment SSH probes. Routine monitoring stops costing agent attention
entirely; the agent is woken only on an escalation the orchestrator cannot
resolve.

What it does not replace in v1: the advertise loop (the agent's own readiness
signal), or the native worker's `reconcile_pending()` (it triggers it, it does
not own it). A heartbeat proxy — the orchestrator advertising for external
agents while their own heartbeat stays fresh — is a named future option, not
v1, because it changes what readiness means.

### 4.4 What does not change

- Claims, leases, advertise, finish, and release stay Postgres functions. The
  0020/0022 external transport is untouched.
- `coding_events` remains the audit log. `verify-task-events.sql` keeps
  working unchanged.
- The native worker's 5-second scan remains as the delivery fallback.
- The portal keeps reading through the existing API and DAO; the orchestrator
  adds one health projection, it does not rewire the Agents section.

## 5. SDLC

The delivery vehicle is the Forge workflow itself: plan, Codex review,
implement, Jenkins verification, guarded merge — one phase per workflow task
set, with Leonardo approving at each phase boundary. Nothing in this design
bypasses the pipeline it improves.

### Phase 1 — Queue infrastructure

- NATS JetStream systemd unit, configuration, data directory, and backup.
- Relay service plus the `relay_progress` table migration.
- Audit of every `coding_events` writer: state transition and event commit in
  one transaction, or the writer is fixed.
- Actor carried in every lifecycle event payload.
- Relay runs in **shadow mode**: it publishes, nothing consumes. Lag metrics
  visible.
- **Gate:** the relay survives `kill -9` and resumes without loss or
  duplicates, verified by an `event_id` dedup audit against the outbox.
  Jenkins CI green.

### Phase 2 — Orchestration service

- Service skeleton, monitors (queue health, stuck records), reconciliation
  triggers, health table, portal surface.
- Runs in parallel with the agent hooks for comparison; the hooks stay
  authoritative until the gate passes.
- **Gate:** injected faults — a stuck lease, a dead relay, a stale profile —
  are detected within one poll interval, with no false escalations over one
  week of parallel running.

### Phase 3 — Cutover

- The native worker and external delivery subscribe to the queue for wake-ups.
  The 5-second scan stays. NOTIFY stays as a secondary wake-up.
- **Gate: the failure drills.** Kill the queue mid-process, kill the worker
  mid-process, kill the relay mid-publish — in one drill run. Zero duplicate
  processing, zero lost work, verified by a status audit: every task's final
  state matches exactly one processing. This drill is the acceptance test for
  the determinism claim in section 1. It is proven, not asserted.

### Phase 4 — Retire

- NOTIFY wake-ups removed. `forge-events-watch`, `forge-heartbeat`, and the
  `HEARTBEAT.md` checks retired. Docs updated.
- **Gate:** one full week of clean operation with the orchestration service as
  the only monitor.

### Rollback

Each phase keeps the previous mechanism live until the next is proven. The
outbox means the queue can always be rebuilt by replaying from the relay
position. The database scan means the queue can be down entirely without
losing work. There is no phase after which the old path cannot be restored by
configuration alone.

### Artifacts and checks per phase

Migrations, systemd units, configuration, relay and orchestrator code, tests,
docs, runbook entries. M27 boundary hashes updated for every deploy file
touched. `make ci` and `make ui-qa` green at every merge.

## 6. Open questions

- Confirm NATS JetStream, or is there a queue already in the homelab this
  trace missed?
- One stream with subjects, or one stream per channel family?
- `coding_events` as the outbox (recommended) versus a dedicated outbox table?
- Portal surface for orchestrator health: the Agents section, or a new Ops
  surface?
- Retention policy for `coding_events`, unbounded today — a separate decision,
  not blocking the split.
- Heartbeat proxy for external agents: future option, or wanted in v1 after
  all?

## 7. Acceptance criteria

- The phase-3 failure drills pass: queue outage plus worker crash plus relay
  crash, with zero duplicate processing and zero lost work.
- The midnight-sweep gap is closed: every lifecycle event carries an actor.
- Agent monitoring hooks retired; routine monitoring costs no agent tokens.
- The orchestration service runs one full week as the only monitor with no
  false escalations and no missed stuck record.
