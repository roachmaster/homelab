# Shared Agent Work Checkpoint

- **task_id:** `agent-plan-progress`
- **objective:** Have coding agents publish an explicit public plan before work and report milestones to PostgreSQL through the supervising worker; show those events in the portal and diagnose the current Running task.
- **authorized_scope:** User requested implementation of agent plan/event reporting. Existing commit, push and portal deployment authorization persists. Preserve the running task, account settings, canonical-source isolation and worker-owned completion checks. No direct database credentials enter the CLI.
- **phase:** `implementing`
- **coordinator:** Codex
- **worktree:** `/home/lrocha/repos/homelab`
- **branch/base:** `main` / `71d845d`
- **ownership:** Root owns API event projection, Rust protocol/model/rendering and integration. Worker author owns executor/worker/progress helper/tests in a separate worktree. SQL author owns additive migration12, verifier/grants and WorkerDB/tests in a separate worktree. Review agent is read-only.
- **decisions:** Reuse checkpoint events with typed public progress payloads. Agent writes bounded sequential reports in its existing scratch scope; parent validates and persists them with token/owner/lease fences and idempotent report ordinals. Reports do not assert completion. Preserve independent task revision and event sequence. Rust renders the current attempt plan and recent milestones; terminal tasks retain readable history.
- **verification:** Initial live check found task 3f7c0936-6a98-4961-9c41-62a3e7ffb50b running with a fresh lease after user resume. Gap reproduced in source: only one starting checkpoint, omitted event payloads, and UI ignoring unchanged-revision checkpoints. Full verification pending final integration.
- **publication_deployment:** Prior release 679088c / ABI7 / policy2 / schema11 remains live. Do not restart a worker with an active task; finish or observe its normal stop first.
- **evidence:** `/mnt/data/homelab-build/agent-progress-20261119/`
- **next_action:** Integrate bounded progress channel, fenced persistence and typed UI; test and deploy after the current task stops.
- **last_updated:** 2026-11-19T22:23:15.497145+00:00
