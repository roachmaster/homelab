# Resubmission queue

Task definitions collected from the Forge database on Mission Control on
2026-11-25, before the planned database deletion, so the work survives the
reset and can be resubmitted into a clean database. Each YAML carries the
full prompt and acceptance criteria exactly as filed (or, for the two
backlog items and the newest defect, as drafted for filing).

The full database history (event stream, tasks, workflows, reviews) is
archived separately in Muse's long-term storage as `forge-history-export/`
(pg_dump plus per-table JSONL) - this directory is only the work queue.

## Layout

- `features/` - feature work still needed; resubmit these.
- `defects/` - defect fixes and defect reports still needed; resubmit these.
- `backlogs/` - backlog items: the backlog-path feature and its first item.
- `archive/` - kept for the record only; do NOT resubmit (each is tied to a
  superseded main tip; the YAMLs say `resubmit: "no"`).

## Index

### Features still needed

- `features/postgres-proxy.yaml` - Build the Postgres proxy microservice (forge-pg-proxy): a simple CRUD pass-through that becomes the only write
- `features/defect-flow.yaml` - Create a first-class defect flow in Forge. Today every submitted task is a feature-style work item; there is n
- `features/base-refresh-override.yaml` - Add a base-refresh override for external (muse-db) workflows. Problem: every muse-db task is claimed against a
- `features/agent-branch-setup.yaml` - Update all Forge agents' instructions to follow the same branch setup Muse uses. Verified 2026-11-25: Muse wor
- `features/branch-cleanup.yaml` - Clean up the remote branches of roachmaster/homelab. Verified 2026-11-25: the remote carries 35 branches -- ma
- `features/portal-correlation-stages.yaml` - Update portal to display a correlation tasks Jenkins cicd stages. This is a fresh run of the work previously a
- `features/drive-snapshot-pipeline-stage.yaml` - Add a final snapshot-refresh stage to the Jenkins pipeline that runs for the homelab main branch only (the mai

### Defects

- `defects/failure-cause.yaml` - Require a cause on failure events. Verified 2026-11-25: workflow 3824445d's round-4 implement attempt (task 51
- `defects/lease-mislabel.yaml` - Fix the 'lease lost' mislabel in the native coding worker. In work_worker.py the SQL renew returns False whene
- `defects/m27-pins.yaml` - Fix forge-m27-update.py duplicating pin entries. The pin updater appends a new pin entry for a file instead of
- `defects/review-cascade.yaml` - Stop review-infrastructure failures from cancelling workflows. Verified 2026-11-25 on workflow b3a05d8a: the i
- `defects/attempt-deadline.yaml` - Make long gate runs survive attempt deadlines. Verified repeatedly 2026-11-25: implement attempts running the 
- `defects/agent-failure-output-discarded.yaml` - Preserve agent output when an attempt fails instead of discarding it

### Backlogs

- `backlogs/backlog-flow.yaml` - backlog-flow
- `backlogs/backlog-recipe-resolution.yaml` - backlog-recipe-resolution

### Archive (do not resubmit)

- `archive/repair-main-verification-2ee824e9.yaml` - Repair failing main verification 2ee824e9a7223c565a7149a9be330b25c3ff84aa.
- `archive/repair-main-verification-32588ab5.yaml` - Repair failing main verification 32588ab5b60006eeaaddf2d1d9ca2a3377fdf893.
- `archive/repair-main-verification-ba37784b.yaml` - Repair failing main verification ba37784b31ba19a7701f9bfa7c48730301a962a4.
- `archive/repair-main-verification-50f9892e.yaml` - Repair failing main verification 50f9892e03dac5318295bd886a32ea5770c46c9a.
- `archive/repair-main-verification-06007c2a.yaml` - Repair failing main verification 06007c2a3450227a2216feb3ca8acc36f086dd9b.

