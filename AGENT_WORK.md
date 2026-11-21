# Shared Agent Work Checkpoint

## Active: Jenkins Rust I/O and Forge CLI — 2026-11-21

- **scope:** Implement an optional reusable Jenkins `IoClient` capability with
  exported typed config, factories and registrar, then consume it through the
  existing Forge CLI's admitted command path. Follow Wire Forge Providers and
  Expose Forge Capability. Preserve existing default/minimal dependency profiles.
- **authorization:** User requested this module and Mission Control credentials.
  The earlier Grok/GitHub workflow run remains stopped; no new remote workflow,
  push or automatic merge is authorized by this task. Native Jenkins installation
  and GitHub service-user access are already verified in the section below.
- **ownership:** Root owns integration/workspace/lock/architecture guard, local
  token provisioning and live read-only acceptance. Separate adapter and CLI
  worktrees own their respective files. Reviewers inspect without writing.
- **source:** Integration is based on local Jenkins provisioning commit `2b92ca5`
  and canonical main `a3968c1`. Live Forge schema17/policy8/ABI9 is unchanged.
- **auth:** Jenkins uses preemptive Basic username/API-token authentication. One
  named Mission Control token was provisioned through Jenkins's existing token
  API and cookie-bound CSRF flow, expires2027-05-20, and inherits the existing
  `forge-admin` account permissions. Its value stays in private files outside Git.
  The CLI config defaults to read-only; that policy does not narrow server rights.
- **progress:** Public adapter skeleton `0870d94` and workspace/lock checkpoints
  are imported. Fake/native transport tests, independent consumer proof, CLI
  admission/help tests, dependency guards and actual CLI-to-Jenkins reads remain
  required. Do not report completion before those checks and independent review.

## Retained earlier checkpoint

- **task_id:** `portal-chat-retirement-muse-20261120`
- **objective:** Remove Chat from the portal while preserving saved data. Install Muse Code, then follow the user's steering to use their consumer Muse email account as the primary integration.
- **authorized_scope:** User requested Chat retirement, Muse installation and agent registration, then selected email as the primary Muse transport at `roachmaster@muse.ai`. Investigate and test that email route with an explicitly scoped first message. Prior deployment/publication authority remains; no subscription purchase or pay-as-you-go fallback. Existing Gemini-author/Codex-reviewer defaults and historical workflows remain.
- **phase:** `candidate_ready_for_autonomous_verification`
- **base:** Canonical main `24dc91c`, runtime/verified source `8a1faf4`, schema17/policy8. Main is clean at task start.
- **ownership:** Root owns integration, installation/login, operational config, final verification and deployment. UI and Muse backend writers use separate worktrees; each has sole file ownership. No receipt-bound task workspace edits.
- **ui:** Typed Chat-disabled portal policy; default and legacy/cached Chat navigation resolve to Agents. Preserve dormant snapshots/drafts/pending hints. Suppress Chat/history/model/proposal I/O and hide all Chat entry points. Existing saved data/backend APIs remain.
- **muse_code:** Official native Muse1.4.24-R9131.1 is installed and signed in; advertised model `muse-spark-1.3` is verified. Official usage returns `allowance:null, reason:no_allowance`; the user confirms no Code subscription and chose consumer email instead. Keep CLI execution inactive. Unfinished native adapter work is preserved on `feat/muse-provider-20261120` at `334db340`; do not ship it or its policy9/migration18. Its latest readiness-fence test failed during PostgreSQL setup. No Meta model calls, purchases or pay-as-you-go fallback occurred; credentials are private and never committed.
- **muse_email:** Muse's welcome email confirms the user's Gmail account as a verified instruction sender to `roachmaster@muse.ai`. Native Codex app-server Apps/MCP tool calls use that connection without a model turn or copied Google credentials. The actual sender stays in private operator configuration. The round-trip evidence below proves the manual transport; a submission receipt alone never establishes delivery or task completion. Persistent automation requires its own durable event/receipt design.
- **email_roundtrip:** A narrowly scoped connectivity email was submitted from Mission Control and received a matching Muse reply in the same Gmail thread, with matching correlation and passing Gmail DKIM/SPF/DMARC results. Muse reported read-only visibility of `roachmaster/homelab`. The user also confirms Muse already has GitHub write access; no write operation was requested or tested. Private addresses, message IDs and operational receipts stay outside Git. The reusable helper is explicit/manual; durable automatic outbox/inbox orchestration remains a labeled design.
- **grok:** The user subsequently requested Grok in the agent mix. Root is preparing the official Grok Build installation and account/capability discovery in parallel. No Grok model call, billing change, native provider or readiness claim belongs to this Chat/Muse-email candidate.
- **policy:** Chat retirement uses the existing schema17/policy8, plus matched local WASM ABI9 assets and an inert workspace restore/save flag. Migrations1–17 and existing recipe/receipt identities stay unchanged. A future external email agent is distinct from an unattended CLI worker and must have its own typed asynchronous lifecycle before it is claimed operational.
- **verification:** Focused writer checks first, then submit the combined reviewed candidate through the normal Gemini-author/Codex-review workflow. Its trusted full repository/browser/backend gates, independent candidate review and required exact-head hosted CI establish publication and source receipts. Deploy matching backend config/migration and atomic generated UI assets after verification and a fresh verified backup/drain.
- **focused_evidence:** Inert workspace restore/save passed 18 HTTP/PostgreSQL tests. The email port passed 17 focused tests, including real fake-process timeout/cleanup and stricter integer RPC identity regressions. Chat-only UI passed all 28 browser groups and deterministic generated-asset/WASM checks; the sanitized email contact passed 27 generator tests and focused browser checks at 320/390/1440px. Full combined verification is still required.
- **next_action:** Ship this reviewed Chat retirement/inert persistence/email-tool snapshot through the existing autonomous workflow and deploy its verified merge. Root continues Grok setup separately. Do not purchase a Muse Code subscription or activate its parked native adapter.
- **last_updated:** 2026-11-20

## Previous completed workflow checkpoint

- **task_id:** `autonomous-agent-review-20261120`
- **objective:** Deliver event-driven independent agent review, bounded repairs and automatic publication/merge; involve the owner for exceptions.
- **authorized_scope:** Gemini authors and implements; Codex independently reviews. The user authorized automatic implementation after Plan approval, up to three Plan revisions, bounded code repairs and automatic merge after independent approval plus all required exact-head checks. Project configuration selects the roles. No paid-credit fallback or automatic account rotation.
- **phase:** `completed_and_deployed`
- **delivery_source:** Verified merge `8a1faf496f76ef30dd6f1362a32e5d70f80f012a` from PR10 is the deployed runtime and receipt-owned source for future tasks. This later documentation checkpoint changes no runtime source or verified merge receipt.
- **ownership:** Root owns final documentation review and publication. The deployment owner completed the runtime upgrade and health verification. Preserve receipt-bound workspaces, source snapshots and historical recovery evidence.
- **completed_workflow:** `7392f0b7-4f67-457a-aa9f-6024d8b5258d` completed on 2026-11-20 at approximately 20:12 UTC. Root task `ce8a730a-68f8-43eb-b466-48e78bb44938` and linked repair `3b272474-ac89-48a8-80df-bd3c56872787` reached accepted completion. Real Codex approved the Plan, then returned `CODEX-FALLBACK-001` against the initial implementation. Gemini automatically performed one code-repair round; real Codex independently approved the repair and attested the finding.
- **automatic_delivery:** The coordinator published PR10, observed all required exact-head hosted checks, merged candidate `3e9498d932cd6b378e4a2e52f2107a47606477ab`, accepted completion and advanced verified-main to `8a1faf496f76ef30dd6f1362a32e5d70f80f012a`. The merge preserves checked tree `94eb012bf4a799578c6941c4ebee1ff733078b53`. Root supplied no manual Plan/code approval or merge decision in this completed path.
- **verification:** `make ci`, all 28 browser acceptance groups and 466 backend tests with zero skips passed. The required hosted checks `check`, `pr-fast-gate` and `real-browser` were green on the exact candidate before merge. The local and hosted evidence applies to its recorded source identities; this documentation checkpoint is not a new runtime test.
- **recovery_boundary:** Operator-led infrastructure repairs and receipt-bound recoveries in PRs 6–9 were needed before this workflow completed. The successful automatic review/repair/publication/merge does not make the entire historical recovery unattended. Failed attempts and their receipts remain historical evidence; no budget reset, fabricated approval or raw status rewrite is claimed.
- **live_state:** M17 and policy `forge-coding-v8` are active from merged source `8a1faf496f76ef30dd6f1362a32e5d70f80f012a`. Strict schema and runner-privilege verification passed; the matching API, coding and publication services are healthy. The catalog reports the worker ready and project base/verified-main at that merge. Portal assets remain at the previously checked UI release; the backend activation did not change them.
- **warm_sessions:** Eligible related author turns use the bounded warm-session path with a 1,800-second TTL; reviewers start fresh. No live cache savings have been measured. Future workflows use policy digest `16b720c601ca3b0667ad60b3e5f5483c0998ba98cdaac4fa5ae35fe46e81c35c`; completed workflow `7392f0b7` retains its historical frozen policy identity (digest prefix `6934`).
- **backup_restore:** The fresh schema 16 backup identified by `ea48b9aa` was verified by an actual restore with 35 tables and 337 constraints. History was preserved; only the documented independent GPU `seen_at` heartbeat difference was allowed.
- **next_action:** No required implementation or activation work remains. Future assigned tasks use the configured autonomous path. This documentation checkpoint submits no additional task and changes no runtime configuration.
- **evidence:** Scoped operational records, including `real-workflow-completion-proof.json` and `codex-warm-activation.json`, remain under `/mnt/data/homelab-build/autonomy-live-20261120/`. Retain the external evidence and its access boundaries; do not commit task results, account data or runtime secrets.

## Implementation and authority contract

- **composition:** Typed factory/provider configuration injects ready context, verification, publication and merge ports. PostgreSQL commits workflow state and leases events. JavaScript renders or performs admitted I/O; Rust owns portal state and actions.
- **review_authority:** Each read-only reviewer receives exact immutable Plan/candidate context. The trusted controller validates structured source-bound approval, revision or escalation evidence before admitting successors. Model prose is not approval, a command or database authority. Author and reviewer remain distinct configured roles.
- **budgets:** The initial Plan may be followed by up to three revised versions; the third revision may still pass. Code repairs use the frozen project budget. Infrastructure, quota and authentication stops are distinct from substantive revisions. Recovery retains approved scope, identities, counters and immutable receipts.
- **merge_authority:** Only independent approval and all required checks on the exact candidate admit trusted automatic merge. Main ancestry, checked tree, publication identity and remote acknowledgement are verified. Uncertain acknowledgements are reconciled by reads. Legacy assignments retain their historical manual lifecycle; missing autonomous context cannot enable manual approval.
- **portal:** Task lists and readers display authoritative workflow phases. Active automatic reviews/checks continue refreshing; completed reviewer tasks show Review complete; escalations show Needs attention. Human cancellation remains capability-gated, and merging is noncancellable. No routine Resume/Approve/Accept prompts appear for autonomous work.
- **CI_trigger_contract:** Every pull request, including backend/docs-only changes, runs all three required checks. Push filters, read-only workflow permissions and actual check execution remain intact; no skipped-success shortcut satisfies the gate.

## Retained historical evidence

- **initial_delivery:** The autonomy implementation began at `bcf154e3cd64477dbd56cf87f3aee23ba42fab6c`; PR3 merged to `69db219` after exact-head checks on `e3224da`. Earlier full local checks included 431 backend tests and 28 browser groups. That pilot imported an exact-hash saved Plan with explicit provenance and used owned GitHub/hosted fixtures; it is separate from the later real automatic PR10 completion. Reports remain under `/mnt/data/homelab-build/autonomous-agent-review-20261120/`.
- **boundary_corrections:** Independent reviews and native/hosted checks corrected merge/cancel fencing, reviewer-title bounds, catalog readiness and Codex sandbox compatibility. Policy 7 uses supported external Codex mode under the enforcing supervisor. An OS probe verified zero capabilities, no-new-privileges, denied protected writes, masked database access and stopped scope.
- **native_deadline_recovery:** Original task `ce8a730a-68f8-43eb-b466-48e78bb44938`, attempt `a6e90dc6-5c93-4420-9018-45ebcdbc4788`, reconciliation `f9596a3d-b9b6-45c1-9ad3-68f5119fa263`. The private once-only recovery retained approved scope and counters; PostgreSQL regressions cover expiry/record-stop races, concurrent idempotency, cap exhaustion and stale or unrelated authority rejection. Public workflow overrides remain blocked.
- **peer_verdict_recovery:** Reviewer task `4c086980-b7a1-4bbf-9f35-03af0f842323`, attempt `fc572966-bf83-4c75-8112-6f3537e8645b`, peer review `196c1901-6aed-49c9-9a41-f1bc562f85b0`. A bounded peer-only correctness-to-source interpretation retained the unchanged revise verdict and open finding; it could not approve or reset budgets.
- **source_delivery_recovery:** Linked repair `3b272474-ac89-48a8-80df-bd3c56872787`, stopped attempt `18946940-aa65-44bb-9737-8eb4bf5b906c`, failed review `7568a7af-1829-44a3-9062-cc43f2f20b57`. Source acquisition, same-lease workspace waiting, once-only stopped-source delivery and serialized review admission retained failure history, infrastructure count 1, source revision and substantive budgets. These infrastructure corrections preceded the later automatic completion.
- **last_updated:** 2026-11-20; completed workflow and final runtime activation verified.
## Operator stop and headless Jenkins — 2026-11-21

- **scope:** User stopped/paused the Grok/GitHub workflow work and requested a
  Jenkins installation job with headless setup. No new GitHub workflow was
  submitted. Grok source, staged artifacts and failed-check evidence remain saved.
- **stopped_run:** The normal owner cancellation API stopped workflow
  `389907bd-42d0-42ed-a693-383e24887405`; its task/result/review history remains.
  No native coding tasks remained queued or running. The pending P8 hotfix and
  Grok P9/schema18 rollout were not deployed. Live Forge remains at `a3968c1`,
  schema17/policy8/ABI9.
- **jenkins:** Native Jenkins LTS2.580.2 and Java21 were installed by a bounded
  local systemd provisioning job. `jenkins.service` is enabled and running, with
  persistent home `/mnt/data/forge-homelab/jenkins` and private loopback port18084.
  A systemd credential created the administrator headlessly. No credential
  contents are in source or job output.
- **verification:** Authenticated API200, anonymous API403, signup disabled,
  inbound TCP agent port disabled, controller executors0, CSRF active, loopback
  binding and actual restart persistence passed. Two initial job failures and
  their corrections are retained in private provisioning evidence. Final local
  job `forge-jenkins-verify-20261120.service` succeeded at23:59:39UTC onNov20.
- **handoff:** See `deploy/mission-control/jenkins/README.md` for access, reviewed
  source and evidence paths. Jenkins build agents/pipelines and replacement Forge
  verification/publication adapters have not been assigned or configured. Do not
  resume the stopped Grok/GitHub work without a new user instruction.
- **github_access_2026-11-21:** At the user's explicit request, the separate
  `connect-github.py` job provisioned the existing operator GitHub token and
  selected SSH identity for Unix user `jenkins`, using its actual home
  `/var/lib/jenkins`. Selected-host credentials remain private0600/0700; target
  writes run permanently as Jenkins UID/GID, with no sudo/group changes. API and
  SSH identify `roachmaster`; GitHub reports homelab read/write permissions; both
  HTTPS and SSH read main `a3968c1`. Job completed at00:11:24UTC. No push, build
  or workflow was started. Jenkins jobs, agents and pipeline wiring remain future.
