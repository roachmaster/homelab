# Proposed external Muse submission storage

This is an unimplemented contract, not an executable migration. No migration
number, database role or consumer is allocated. Migrations1–19 and their strict
catalog/privilege verifiers remain unchanged.

| Field group | Required ownership |
| --- | --- |
| Submission identity | Authenticated producer, permitted owner/project, correlation, kind and immutable payload digest |
| Target binding | Task and expected revision, exact source/plan/content digests |
| Lifecycle | Server-owned state, generation, bounded attempts, claim owner and expiry |
| Claim fence | Fresh unpredictable token checked on every mutation |
| Completion | Immutable receipt and event committed atomically |

A producer would receive a typed authenticated submission operation, never
broad INSERT or native worker/reconciler access. Future scoped columns or an
admitted function must reject caller-supplied lifecycle state. UUIDs alone do
not authorize a task. Claim/renew/finish/release must enforce owner, generation,
token, expiry and retry limits so stale claimants cannot affect reclaimed rows.

Idempotency covers the complete submission intent: producer, correlation,
owner, project, task, expected revision, kind and payload digest. Reusing a
correlation with unchanged text and a changed task or owner is a conflict.
Authorization precedes receipt lookup/replay; an existing row UUID is not a
read capability. A producer cannot select an owner or project outside its grants.

Progress must validate and lock the task, bind its expected revision and state,
then allocate its event sequence and complete the intake row in one transaction.
Completion must check every lease predicate and the compare-and-set result.
Every future privileged SQL function needs exact catalog/body/ACL expectations
and negative drift tests, alongside expiry, replay and cross-owner tests.

The later migration0020 feature proposal remains unaccepted; it is not copied
into executable migrations. See the [reviewed gaps](../muse-agent.md#review-of-the-proposed-migration0020-revision).

Payload validation and text hashes cannot stand in for external execution or
source evidence. No task transition, event, review or reconciliation occurs
until that receipt scheme is implemented, independently reviewed and tested.
