---
name: muse-agent
description: Integrate or review the external Muse database provider. Muse owns its runner; Forge admits scoped database assignments and honest artifacts without launching Muse CLI or email.
---

# External Muse database provider

Start with [Wire Forge Providers](../wire-forge-providers/SKILL.md) and the
[Muse DB contract](../../../docs/architecture/muse-agent.md).

Use `muse-db-primary` with resource `forge-external-db-v2`, explicit enablement,
model metadata `external-runner` and portal transport `external_db`. Native
`muse-primary` remains disabled under frozen P10. Do not enable the old
[external intake prototype](../../../docs/architecture/proposals/muse-intake-rejected.md).
Preserve historical v1 recipes. For automatic SDLC use the scoped
[external workflow contract](../../../docs/architecture/external-agent-workflow.md):
Muse authors, an independently configured native agent reviews, bounded author
revisions precede Jenkins verification and a guarded automatic merge. An older
saved v1 Plan can be adopted without rewriting its recipe or artifact.

The external runner owns execution and uses the scoped PostgreSQL capabilities,
advertise, claim, renew/progress, finish/release and artifact functions. Preserve
the existing read permissions/default and narrow authentication rule. Scope
checks precede replay; every mutation requires the bound principal, task intent,
generation, token, revision, lease and deadline. Notifications only wake a durable
queue scan. Never give the runner arbitrary table-write or native worker access.

Bootstrap selects effect-free factories and injects a narrow ExternalWorkPort.
Task/event/assignment admission is atomic inside the owning WorkStore cursor.
The existing coordinator owns bounded database-only expiry maintenance, including
when the portal is closed. No local Muse CLI process or email/usage poller is
part of this provider.

External Plan/Review artifacts appear as `external_result` with `provider:
muse-db`, their actual digest/content/time and bounded summary. Never manufacture
native attempts, systemd stop proofs or workspace reconciliation receipts. The
256KiB artifact and 4KiB summary limits protect report transport, not model token
budgets. The M22 coordinator owns database-only review/implementation transitions;
a separate leased delivery worker owns Git/Jenkins/merge credentials. Enable
`external_reviews` only on the independently configured native worker. Its actual
attempt and stop/reconciliation evidence must bind the full immutable external
artifact. Supply and verify the complete claim context/digest, approved Plan,
feedback, trusted base and candidate result contract on every author follow-up.

M21 gives every root task a durable correlation ID and every event its own UUID.
The portal's bounded history shows external assignment progress, native linked
review tasks and coordinator checkpoints with their truthful source. A Plan's
`awaiting_review` status is neither approval nor completion. Confirm the actual
review verdict, all fixed exact-source Jenkins checks and main receipt before
reporting a workflow complete. Preserve the stable delivery action on uncertain
acknowledgements; never blindly resubmit a Jenkins job or manufacture a receipt.

Preserve native AGY/Codex workflows and historical recipes. Keep idempotent task
receipt lookup before current admission checks. Test selected-instance use,
atomic rollback, cross-scope and stale-lease denial, honest readiness/artifacts,
response bounds and shutdown using fake ports and isolated PostgreSQL.
