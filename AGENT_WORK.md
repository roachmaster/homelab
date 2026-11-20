# Shared Agent Work Checkpoint

- **task_id:** `agent-feedback-loop-design-20261120`
- **objective:** Review the Gemini CI execution and design a feedback loop that lets the original agent repair verified findings before coordinator takeover.
- **authorized_scope:** User requested review and determination of the feedback workflow. Design and diagnostic preparation only; no new Gemini attempt, runtime change, deployment, main merge or paid fallback.
- **phase:** `design_complete`
- **coordinator:** Codex
- **worktree/base:** Isolated `design/agent-feedback-loop` worktree at `/mnt/data/homelab-build/agent-feedback-loop-20261120/design`, main base `b054b5f`.
- **ownership:** Root owns this documentation worktree. Read-only reviewers trace context delivery, admission/publication state, hosted failure and repair policy. Canonical main and receipt-bound native workspaces remain unchanged.
- **findings:** Resume.note is saved but not delivered as independent execution context; original Plan note is rendered instead. Publication starts without a critic/repair round, including missed-callback recovery. Completed tasks cannot resume; terminal publication runs lack retry admission. Local acceptance preceded a later hosted Rust1.100 failure; local Rust1.97 evidence did not close that check.
- **design:** Persist source-bound ReviewFeedback and lease-bound reader, inject separate feedback context, run independent verification, classify source/infrastructure failures, return source corrections to Gemini first, preserve original plan and source receipts, and gate completion on exact-head required hosted checks. Proposed three repair rounds, handoff after two successive rounds without verified progress. No automatic account rotation, broader access or gate weakening.
- **verification:** Source/DB/API paths and previous run artifacts reviewed; diagnostic packet for the current Rust1.100 Infallible lint prepared. All relative documentation links passed; an independent reviewer approved diagram ordering and authority boundaries with no blocking findings. No runtime tests are claimed for a design-only change.
- **evidence:** Prior run `/mnt/data/homelab-build/gemini-cicd-execution-20261120/`; pilot packet `audit/hosted-correction-feedback.json`.
- **deliverable:** `docs/architecture/proposals/forge-agent-feedback-loop.md`
- **next_action:** Implement the three proposed stages only when requested; use the prepared hosted compiler diagnostic as the pilot. The actual automated loop and linked repair submission are not implemented or started.
- **last_updated:** 2026-11-20
