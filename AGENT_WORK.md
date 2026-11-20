# Shared Agent Work Checkpoint

- **task_id:** `plan-resume-revise-cancel`
- **objective:** Implement finished-plan Resume/Revise/Cancel decisions, approved implementation, isolated verification and trusted PR publication targeting main, then deploy the portal lifecycle.
- **authorized_scope:** User explicitly requested immediate implementation of the reviewed flow. Preserve stored plans/results, accepted receipt-bound workspaces and unrelated services. Resume authorizes implementation plus checked feature-branch PR publication; merge remains separate. No automatic submission of the existing operator plan, paid test model calls, main force-push or automatic merge.
- **phase:** `verifying`
- **coordinator:** Codex
- **worktree/base:** `/home/lrocha/repos/homelab` / `444e0f8`
- **ownership:** Root owns worker plan-context/publisher handoff, isolation changes, docs, integration, full gates and deployment. Isolated backend author owns typed plan decisions, policy4 and additive migration14/private plan and publication DB routines. Frontend author owns Rust/WASM/native relay/generator/browser contracts. Publication author owns selected trusted snapshot/Git/check/PR providers and publication service/tests.
- **decisions:** New typed ide.work.plan.decide route binds current revision and exact published plan artifact. Resume queues Implement, Revise queues Plan with feedback, Cancel preserves history and stops workflow; legacy accepted Plan remains actionable. Mode advance is explicit and historical attempt recipes remain immutable. Agent CLI remains unable to publish; a trusted factory-wired publisher uses a separate verified snapshot, fixed checks and generated branch/PR receipts. Checks receive no native CLI/GitHub/DB credentials. Policy4/M14 preserve prior policies/records.
- **verification:** Initial tree clean. Provider, command-flow, modern UI and generated-UI/testing skills reviewed. Existing gh authentication is available; no new secret or model call is needed. Integrated artifact-bound decisions, immutable plan handoff and isolated trusted publication. First full backend287 run exposed one old Plan-Accept progress fixture; updated it to Review and its six regressions passed. Actual systemd verification isolation passed. Full exact-source backend, fixed make ci and all28 browser groups are the rollout gate.
- **publication_deployment:** Current live web1be9429, API/worker aa2eb1a, schema12/policy2. Coordinated additive schema13/14 and policy4 deployment will preserve data and drain any running assignments before service switch.
- **evidence:** `/mnt/data/homelab-build/plan-lifecycle-20261120/`
- **next_action:** Complete exact-source gates, review rendered plan controls, commit/push and deploy schema13/14 plus policy4 and publisher without executing the user's saved plan automatically.
- **last_updated:** 2026-11-20T00:27:32.970428+00:00
