# Proposed external Muse submission storage

This is an unimplemented contract, not an executable migration. No migration
number, database role or consumer is allocated. Migrations1–19 and their strict
catalog/privilege verifiers remain unchanged.

| Field group | Required ownership |
| --- | --- |
| Submission identity | Authenticated producer, owner/project, correlation and immutable payload digest |
| Target binding | Task and expected revision, exact source/plan/content digests |
| Lifecycle | Server-owned state, generation, bounded attempts, claim owner and expiry |
| Claim fence | Fresh unpredictable token checked on every mutation |
| Completion | Immutable receipt and event committed atomically |

A producer would receive a typed authenticated submission operation, never
broad INSERT or native worker/reconciler access. Future scoped columns or an
admitted function must reject caller-supplied lifecycle state. UUIDs alone do
not authorize a task. Claim/renew/finish/release must enforce owner, generation,
token, expiry and retry limits so stale claimants cannot affect reclaimed rows.

Payload validation and text hashes cannot stand in for external execution or
source evidence. No task transition, event, review or reconciliation occurs
until that receipt scheme is implemented, independently reviewed and tested.
