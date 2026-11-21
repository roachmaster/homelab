---
name: muse-agent
description: Review or extend the proposed external Muse intake design. Its DTOs and validators are dormant; use the manual email transport for current Muse assignments.
---

# External Muse intake design

Start with [Wire Forge Providers](../wire-forge-providers/SKILL.md) and the
[Muse intake proposal](../../../docs/architecture/muse-agent.md).

Email remains the primary, operator-invoked Muse integration. The separately
merged native Muse Code adapter defaults disabled. External email intake has no
live watcher, database login, schema migration, worker registration or service.
Its consumer and store operations reject before acquiring ports. Changing a
profile or borrowing existing worker credentials cannot enable this scaffold.

A future implementation needs a typed external-result receipt with authenticated
producer, owner/project/task revision, exact source/content digests and genuine
execution provenance. Never invent a systemd stop proof or substitute a text
result hash for a workspace reconciliation digest.

Review claim-token fencing, bounded expiry retries, scoped submission columns,
atomic event/receipt commits, duplicate replay and cancellation before admission.
Keep external data separate from native work profiles and frozen P10 recipes.
An externally supplied plan must become a bound independent-review artifact,
not a new plan prompt waiting for its own result.
