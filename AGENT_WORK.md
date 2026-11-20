# Shared Agent Work Checkpoint

- **task_id:** `autonomous-agent-review-20261120`
- **objective:** Deliver event-driven cross-agent planning, implementation review and automatic publication/merge, with human escalation after bounded revisions.
- **authorized_scope:** User corrected the prior human-driven design: Gemini authors a plan, an event assigns Codex to review, approval automatically starts execution, and revisions return to the author up to three times before human escalation. User explicitly chose automatic merging after agent approval and all CI checks. Roles must be configurable for future Muse integration. No paid-credit fallback or automatic account rotation.
- **phase:** `contract_and_design`
- **base:** Main `bcf154e3cd64477dbd56cf87f3aee23ba42fab6c`; clean source. Previous feature passed381 backend tests and28 browser groups before its merge. Existing live portal/schema/services remain on the older release.
- **ownership:** Root owns contracts, design, integration and independent verification. Backend/runtime/UI reviewers are proposing narrow interfaces read-only before parallel implementation. Each writer will use an isolated worktree.
- **design_direction:** Persist a frozen project workflow policy with distinct author/reviewer roles and delegated authority. PostgreSQL commits and leases control event handling. A reviewer receives immutable exact plan/candidate context; typed source-bound approve/revise/escalate results are validated by a trusted coordinator before transitions. Reviewer output is never a raw command or direct database privilege. Preserve completed history and all native receipt-bound workspaces.
- **revision_semantics:** Initial proposal plus up to three revised versions. The third revised version may still pass review; another revision request escalates. Infrastructure/auth/quota failures are separate from substantive revision counts.
- **merge_authority:** Automatic merge is explicitly authorized only for a configured autonomous workflow with independent reviewer approval and all required checks on the exact candidate. Preserve main history, respect repository protection and prevent unverified source/base races. Reconcile uncertain remote acknowledgements by reads.
- **next_action:** Freeze shared DTO/DAO/event contracts, implement normal factory/provider paths, test real database transitions and reviewer delivery, update portal status/design pages and verify the complete workflow.
- **last_updated:** 2026-11-20T13:19:53.535110+00:00
