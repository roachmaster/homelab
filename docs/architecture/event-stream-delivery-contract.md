# Event-stream delivery contract (interim)

This is the standing contract for what the Forge event stream does and does
not promise, until the queue split lands. It writes down what is true today
so that the peer-review diagnostics (fix 1), actor/cause attribution (fix 2),
and the Jenkins lifecycle producer (fix 3) — and all future delivery work —
build on the same assumption. It changes no mechanism.

## The assumption fixes 1–3 rely on

- A lifecycle transition is durable when its row is committed. Delivery of
  the *notification* is best-effort. No fix in this plan assumes a consumer
  learns of a transition from a notification; every consumer that must act
  reads the durable row (task, workflow, review, or observation record).
- Event payloads are the audit record, not the delivery vehicle. Fix 2's
  actor and cause fields serve audit and diagnosis; they do not change what
  any consumer is woken for.

## The interim contract

- **NOTIFY is a wake-up hint only: empty payload, coalesced by Postgres, may
  be lost.** Every `pg_notify` in the migration tree carries an empty payload
  (`pg_notify('forge_coding_jobs', '')`, and likewise for
  `forge_agent_events` and `forge_agent_results`). No consumer may treat the
  absence of a notification as the absence of a transition, and no consumer
  may treat a notification as carrying data.
- **The 5-second DB scan is the delivery mechanism, with the 5 seconds as
  the IDLE polling timeout** — the wait applied when the worker has nothing
  to do. `PublicationWorker.run_forever` (`deploy/mission-control/
  agent-runner/work_publication.py`) calls `self.store.wait(committed, 5)`
  only after `run_once()` returns False, and loops back to `run_once()` (the
  scan) when the wait returns. A busy worker defers the next scan for the
  duration of its current assignment (`run_once` calls `process(lease)`
  synchronously), so **no latency bound is claimed for a busy worker**.
  Scheduling latency is qualified by processing time (how long the current
  assignment runs), capacity (how many workers are running), and
  reconciliation (what the coordinator must recheck before the next scan).
  The contract states the timeout and the qualifiers; it does not promise a
  number the loop cannot keep.
- **Current-state convergence, not per-transition observation.** The scan
  re-reads current state, so a consumer that crashed and restarted converges
  to the current state without any replay — but intermediate transitions can
  disappear: a task that moves queued → running → awaiting_review between
  two scans is seen only in its final state. The contract guarantees eventual
  convergence to current state for a healthy worker, not observation of every
  transition. Anything that needs every transition must read the event log
  (`coding_events`), not the scan.
- **Consumers must be idempotent on state, not on events:** claiming a task,
  recording a verdict, or emitting a notification must be safe to repeat
  after a crash, because the scan will present the same state again.

## What replaces this contract

The queue-split design (`docs/architecture/proposals/postgres-queue-split.md`)
replaces the scan with true delivery (outbox relay + durable queue). Until
it lands, this contract is the whole delivery story, and any delivery work
that assumes more than it is building on a falsehood.

## Verification

`deploy/mission-control/agent-runner/test_work_delivery_contract.py`
asserts the three testable guarantees: every `pg_notify` payload in the
migration tree is empty, the idle scan timeout is 5 seconds, and a missed
notification still converges to current state via the scan. It asserts no
latency bound for a busy worker — only the structural fact that the scan is
deferred while the worker is busy.
