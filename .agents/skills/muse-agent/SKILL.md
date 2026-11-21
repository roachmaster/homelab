---
name: muse-agent
description: Integrate or review the external Muse database provider. Muse owns its runner; Forge admits scoped database assignments and honest artifacts without launching Muse CLI or email.
---

# External Muse database provider

Start with [Wire Forge Providers](../wire-forge-providers/SKILL.md) and the
[Muse DB contract](../../../docs/architecture/muse-agent.md).

Use `muse-db-primary` with resource `forge-external-db-v1`, explicit enablement,
model metadata `external-runner` and portal transport `external_db`. Native
`muse-primary` remains disabled under frozen P10. Do not enable the old
[external intake prototype](../../../docs/architecture/proposals/muse-intake-rejected.md).

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
budgets. Implement, independent artifact-context review and Jenkins/candidate
integration require a separate bridge; awaiting_review is not approval or merge.
Progress summaries remain database-only in this slice; portal task status and
the final external artifact are visible without inventing native progress IDs.

Preserve native AGY/Codex workflows and historical recipes. Keep idempotent task
receipt lookup before current admission checks. Test selected-instance use,
atomic rollback, cross-scope and stale-lease denial, honest readiness/artifacts,
response bounds and shutdown using fake ports and isolated PostgreSQL.
