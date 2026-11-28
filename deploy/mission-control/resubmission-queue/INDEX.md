# Resubmission queue

Task definitions collected from the Forge database on Mission Control on 2026-11-25, before the planned database deletion, so every task that still needs to run can be resubmitted afterwards. `resubmit: "no"` marks tasks tied to superseded main tips - do not resubmit those as-is.

- `postgres-proxy.yaml` - resubmit: yes - status at collection: awaiting_review - Build the Postgres proxy microservice (forge-pg-proxy): a simple CRUD pass-through that becomes the 
- `lease-mislabel.yaml` - resubmit: yes - status at collection: cancelled - Fix the 'lease lost' mislabel in the native coding worker. In work_worker.py the SQL renew returns F
- `agent-branch-setup.yaml` - resubmit: yes - status at collection: cancelled - Update all Forge agents' instructions to follow the same branch setup Muse uses. Verified 2026-11-25
- `branch-cleanup.yaml` - resubmit: yes - status at collection: cancelled - Clean up the remote branches of roachmaster/homelab. Verified 2026-11-25: the remote carries 35 bran
- `defect-flow.yaml` - resubmit: yes - status at collection: cancelled - Create a first-class defect flow in Forge. Today every submitted task is a feature-style work item; 
- `repair-main-verification-2ee824e9.yaml` - resubmit: no - status at collection: cancelled - Repair failing main verification 2ee824e9a7223c565a7149a9be330b25c3ff84aa.
- `repair-main-verification-ba37784b.yaml` - resubmit: no - status at collection: cancelled - Repair failing main verification ba37784b31ba19a7701f9bfa7c48730301a962a4.
- `repair-main-verification-32588ab5.yaml` - resubmit: no - status at collection: cancelled - Repair failing main verification 32588ab5b60006eeaaddf2d1d9ca2a3377fdf893.
- `drive-snapshot-pipeline-stage.yaml` - resubmit: yes - status at collection: cancelled - Add a final snapshot-refresh stage to the Jenkins pipeline that runs for the homelab main branch onl
- `repair-main-verification-50f9892e.yaml` - resubmit: no - status at collection: cancelled - Repair failing main verification 50f9892e03dac5318295bd886a32ea5770c46c9a.
- `attempt-deadline.yaml` - resubmit: yes - status at collection: awaiting_review - Make long gate runs survive attempt deadlines. Verified repeatedly 2026-11-25: implement attempts ru
- `portal-correlation-stages.yaml` - resubmit: yes - status at collection: cancelled - Update portal to display a correlation tasks Jenkins cicd stages. This is a fresh run of the work pr
- `repair-main-verification-06007c2a.yaml` - resubmit: no - status at collection: cancelled - Repair failing main verification 06007c2a3450227a2216feb3ca8acc36f086dd9b.
- `failure-cause.yaml` - resubmit: yes - status at collection: cancelled - Require a cause on failure events. Verified 2026-11-25: workflow 3824445d's round-4 implement attemp
- `m27-pins.yaml` - resubmit: yes - status at collection: cancelled - Fix forge-m27-update.py duplicating pin entries. The pin updater appends a new pin entry for a file 
- `review-cascade.yaml` - resubmit: yes - status at collection: awaiting_review - Stop review-infrastructure failures from cancelling workflows. Verified 2026-11-25 on workflow b3a05
- `base-refresh-override.yaml` - resubmit: yes - status at collection: failed - Add a base-refresh override for external (muse-db) workflows. Problem: every muse-db task is claimed
- `backlog-flow.yaml` - resubmit: yes - status at collection: drafted-never-filed - backlog-flow
- `backlog-recipe-resolution.yaml` - resubmit: yes - status at collection: drafted-never-filed - backlog-recipe-resolution
- `agent-failure-output-discarded.yaml` - resubmit: yes - status at collection: drafted-never-filed - Preserve agent output when an attempt fails instead of discarding it
- `workflow-recovery-revival.yaml` - resubmit: yes - status at collection: drafted-never-filed - Restore a sanctioned revival path for disabled, escalated workflows (issues #24/#27)
- `manifest-return-loop.yaml` - resubmit: yes - status at collection: drafted-never-filed - Stop the external-workflow manifest-return loop
- `dispatch-digest-diagnosis.yaml` - resubmit: yes - status at collection: drafted-never-filed - Make dispatch result_mismatch diagnosable: stale base vs stale digest vs revoked grant
