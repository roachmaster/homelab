# Shared Agent Work Checkpoint

## Current: Native Gemini workflow on Jenkins — 2026-11-21

- **authorization:** User requests updating Gemini's native workflow after the
  identified legacy GitHub-check dependency. Use Jenkins for full verification,
  keep independent Codex review and automatic guarded merge. GitHub Actions
  remains disabled. Preserve current tasks, immutable history and review budgets.
- **ownership:** Separate worktrees own the injected native Jenkins provider,
  scoped cancellation/status persistence checks and operational review. Root
  owns configuration, integration, documentation, verification and activation.
- **implementation:** Explicit publisher selection injects one native Jenkins
  owner for local checks and later receipt observation. The three frozen logical
  gate IDs, execution policies and workflow digest stay unchanged. A private
  binding connects the native review manifest to the separate exact-source
  Jenkins manifest, actual workflow/task/attempt/correlation and durable action.
  Reuse the Rust Jenkins IoClient and installed trusted supervisor. Run every
  fixed gate once per admitted verification action; publication and merge
  reobserve its genuine receipt. A separately admitted bounded verification
  retry can run again. New M24 APIs expose only scoped CI identity/status, with
  no direct table-read grant. Fair restart scanning, exact bundle recovery,
  conservative post-submit cleanup and notification draining are implemented.
- **source:** Native publication already imports and merges actual remote main
  before checks and independent code review. Keep the old author source receipt
  and verified-main reference until a real successful native merge advances it.
  No raw workflow snapshot edits, fabricated approvals or budget resets.
- **active task:** Cache task `81f71bf0-5d92-4fed-b282-f8fd51f27f49`
  passed independent Plan review after three revisions and started implementing
  at18:24 UTC. Its frozen deadline stopped attempt `daba0ba7` at18:54:12. The
  existing event-driven recovery automatically resumed the same task at18:54:18
  in attempt `f4154965`, revision17, preserving the work and Plan counters. No
  manual resume, reset or approval was supplied. The publisher was stopped while
  idle for this cutover; the author, API and other services keep their identities.
- **verification:** Independent SQL/native regressions60/60 and final affected
  backend tests67/67 passed. Independent final provider review passed65/65;
  six offline deployment-template checks passed. Full backend discovery passed
  716/716 in400.918s; the final67-test run covers the later scope/listener
  refinement. New module is included in normal setup-runner installation.
  Rust tree and ABI12 assets remain byte-identical; no Rust
  rebuild or new browser run is claimed for this Python/SQL change.
- **deployment:** Activate only after the final reviewed source is on remote
  main, strict schema24 verification and a fresh owned backup/restore pass.
  The schema23 pre-DDL proof is run `38e6e3fa944f`,45 tables, strict=true. An
  earlier cleanup timeout remains failed evidence; only a confirmed dedicated
  LISTEN connection was cleared, leaving Gemini's model/lease intact. Deploy
  only publisher source/config plus the service-local Jenkins group and spool
  grant; preserve other services and the installed binary/helper. Activation
  receipts stay private so a later documentation push cannot invalidate the
  active candidate's exact-main ancestry. Actual production Jenkins/cold-warm
  cache success must be proven separately, never inferred from fixture tests.
- **evidence:** `/mnt/data/homelab-build/native-jenkins-20261121`.

## Current: Unattended Gemini execution and task monitoring — 2026-11-21

- **authorization:** User requests monitoring the submitted tasks and making
  Gemini run with full access, then explicitly directs inspection of
  `work_executor.py`. Existing task publication/review/CI rules remain in force.
- **observed cause:** Cache-planning task
  `b5f3e148-1857-427c-8f26-6afa91a2093c` stopped with `blocked_permission`
  at17:11. Its retained AGY conversation identifies a denied ReadFile of
  `/usr/local/libexec/forge-ci-check`, outside the generated tool read allowlist.
  The installed AGY1.5.12 help verifies `--dangerously-skip-permissions`; P10
  deliberately excludes that flag and must retain its historical identity.
- **implementation:** Add the separately versioned
  `forge-coding-agy-unattended-v1` policy and explicit worker selection. Keep
  native P10, Codex reviewer execution and frozen Muse workflow recipes intact.
  New Gemini tasks use unattended CLI approval under the existing outer process
  boundary. Plan/review source stays read-only; implementation remains owned
  by the approved task workspace. No root account or publication bypass.
- **ownership:** Backend policy/adapter work, additive SQL migration23 and
  deployment templates have separate writers/worktrees. Root integrates,
  reviews, verifies and deploys. The blocked root owns an immutable P10 native
  workflow, so generic Resume is correctly refused. After a genuine fix, use
  ordinary Submit for a versioned replacement with the same original prompt and
  acceptance criteria, then Cancel the already stopped predecessor through the
  normal revision/idempotency API. Preserve all failed-run history.
- **delivery:** Source `9139bf7` is published and its API/Gemini worker are live
  as release `20261121T175111Z-9139bf7-main`, schema23. Existing P10 worker,
  publisher, external delivery, host, source agent and Jenkins process identities
  were preserved. The dedicated worker shares trusted task metadata/workspace
  roots with the existing publisher and has its own worker ID/readiness. Each
  model still sees only its own isolated attempt scratch. Old `ask: []` settings
  alone must not be described as full CLI auto-approval.
- **actual recovery:** Normal portal Submit created replacement cache task
  `81f71bf0-5d92-4fed-b282-f8fd51f27f49`, then normal Cancel retired the stopped
  P10 predecessor. Its attempt/history/workspace and frozen workflow remain.
  Actual attempt `3529f7aa-5daf-4642-b6b3-7f76c617dd85` ran with
  `--dangerously-skip-permissions`; live PID/mount inspection confirmed UID1000,
  zero effective capabilities, NoNewPrivs and read-only settings/Plan workspace.
  The CLI explicitly logged auto-approval and the previously denied installed
  CI-file read succeeded. Its first 24,375-byte Plan finished at18:06 UTC.
  A genuine Codex review requested a branch-cache correction; Gemini resumed
  automatically in Plan revision1, still under the new policy.
- **source verification:** Integrated backend suite695/695 passed (411.462s),
  followed by the final six PostgreSQL cases including the additional native
  review/feedback regression. Independent backend and SQL reviews approved the
  change. P10 policy/digest and Rust tree `f4fb31cf271e3799688f539e4b28b5e7d96f7800`
  are unchanged. Reuse only the matching previously verified Rust binaries and
  ABI12 assets; do not claim a new full Jenkins build for this Python/SQL fix.
  A fresh strict schema22 backup/owned restore passed before production DDL.
  A fresh strict schema23 backup/owned restore also passed before activation;
  all45 tables, ownership, roles/credentials/ACL/defaults/HBA and old migration
  rows were preserved. The old live-host probe's unconditional online-Muse
  assertion fails legitimately when Muse stops advertising; do not fabricate a
  heartbeat to make it green. At18:05 its last genuine heartbeat was17:59 UTC,
  with both the external repair and archive Plan still queued.
- **evidence:** `/mnt/data/homelab-build/gemini-unattended-20261121`.
- **source/CI boundary:** Native admission is still bound to the actual
  `a3968c1` merge receipt and unchanged `refs/forge/verified-main`. Give the
  replacement explicit read-only context for the current f26 deployed Jenkins
  helper; do not pretend its checkout is current main. The native workflow's
  configured hosted-check names still target the legacy GitHub path; only the
  external Muse workflow currently has trusted Jenkins delivery. Do not enable
  GitHub Actions or describe this permission fix as native Jenkins integration.

## Current: Complete the agent workflow and event history — 2026-11-21

- **authorization:** User explicitly requests finishing the system rather than
  returning more partial work. Implement the Muse-to-independent-review handoff,
  bounded revision/implementation/verification/merge flow, and the reviewed task
  correlation/event-history design. Existing authorization includes independent
  agent review, at most three revisions and automatic merge after real CI.
- **ownership:** Separate worktrees own SQL trace migration21, external workflow
  backend/migration22, and Rust/generated UI/trace projections. Root integrates,
  supplies trusted candidate/Jenkins/merge delivery, verifies and deploys. The
  native author's receipt-bound source base is older than manual main; preserve
  that authority and use explicit external context instead of fabricated receipts.
- **boundaries:** Muse owns its runner through PostgreSQL; native Muse CLI stays
  disabled. Preserve existing roles/reads/defaults/HBA, frozen policies and
  migrations1–20, immutable artifacts and all historical receipts. No global
  registry lookups in workers. JS remains generated presentation/I/O glue.
  GitHub Actions stays disabled. Verification must use Jenkins with exact source
  identity, followed by an independently approved, guarded merge.
- **delivery:** In progress. Recover actual external Plan task
  `7699599c-b5d0-4118-9bb0-3f8f917c5843` through genuine reviewer work and show its
  lifecycle in the portal. Do not manually mark it approved/completed or invent
  stopped-process, workspace, CI or publication evidence.
- **evidence:** `/mnt/data/homelab-build/complete-agent-workflow-20261121`.

## Retained: Merge Muse's completed plan — 2026-11-21

- **authorization:** User requested merging Muse's work. GitHub has no Muse
  feature branch or open PR; the completed deliverable is its immutable Plan
  artifact for task `7699599c-b5d0-4118-9bb0-3f8f917c5843`.
- **source:** The 14,180-byte artifact was verified against SHA256
  `9bb7fc6d7b937f6fd4213fd09644e2a2e81af4e1ce93823519616712033455aa`.
  Original content and provenance remain in PostgreSQL and private evidence
  under `/mnt/data/homelab-build/merge-muse-plan-20261121`.
- **delivery:** Import the reviewed task-correlation/event-envelope proposal and
  link it from the documentation indexes. Retain Muse's design while correcting
  bounded paging, strict request/response compatibility, external progress,
  migration locks/verifiers and frozen intent identities. This is a design merge;
  no migration, runtime implementation or deployment belongs to this change.
- **runtime observation:** Muse has submitted its plan and advertised a real
  heartbeat. The task remains `awaiting_review` without a workflow/reviewer link.
  Publishing this source proposal does not manufacture a native review receipt,
  approve implementation or change database task state. The external-to-review
  runtime bridge remains unfinished. Preserve the live schema20/ABI11/P10 release,
  Muse access, verified-main ref and the stop on GitHub Actions.
- **verification:** Independent review approved the proposal after all substantive
  findings were corrected. Original-artifact digest, UTF-8/LF,75 local links and
  Git whitespace checks passed. Runtime code is unchanged; no migration, model
  task, service change or database write was performed for this merge.

## Retained: External Muse database handoff — 2026-11-21

- **authorization:** User selected an independently operated Muse runner that
  monitors PostgreSQL. Set up the database integration directly; do not launch
  Muse Code or email. Preserve the existing Muse login, reads, defaults and HBA.
- **ownership:** Root integrates Rust/UI and deploys. Separate worktrees own
  the additive SQL contract and the Python provider. An independent reviewer
  checks fencing, privileges and the verified-backup helper.
- **boundary:** External assignments and artifacts use their own provider and
  receipts. They never manufacture native attempts, stopped-process evidence,
  workspace reconciliation, review approvals or CI results. Native Muse stays
  disabled. GitHub Actions stays disabled; Jenkins orchestration remains separate.
- **delivery:** Code `2cd504d` is published on main and deployed as immutable
  release `20261121T044052Z-2cd504d-main`: schema20, matched ABI11 and unchanged
  native P10. All five existing application services are active and enabled.
  The scoped `muse-db-primary` binding is enabled for Homelab; authenticated
  Muse capabilities and private-storage denial were checked through its actual
  PostgreSQL principal. No Muse process or email watcher was installed.
- **runner state:** No external heartbeat has arrived. The portal correctly
  shows No runner heartbeat and exposes the task option only after genuine
  advertisement. Mission Control did not advertise on Muse's behalf or submit
  a model task. Muse must use the documented database contract from its runner.
- **verification:** Integrated external tests23/23, native policy/history upgrade
  tests20/20, final UI unit tests113/113, generation tests27/27, targeted Rust
  package tests, Clippy and deterministic assets passed. All28 sandboxed browser
  acceptance groups passed with successful cleanup. Live asset/session/CSRF,
  mTLS Workbench and isolated browser checks through the existing loopback proxy
  passed; external Cloudflare sign-in was not exercised.
- **data and limits:** Verified schema19 backup preceded migration. All existing
  rows, Muse account/password, thirteen table reads, future-table default and HBA
  were preserved. A fresh strict schema20 backup was restored and compared
  across all39 tables. This transport admits plans and review reports; progress
  text is database-only. Independent external-artifact review/context staging,
  implementation and Jenkins-to-merge wiring remain separate unfinished work.
  Native Muse Code stays disabled, the receipt-bound verified-main ref is
  unchanged, and GitHub Actions remains disabled.
- **evidence:** `/mnt/data/homelab-build/muse-db-enable-20261121`.

## Current: Main deployed with preserved Muse access — 2026-11-21

- **authorization:** User requested merging the remaining branches and following
  deployment instructions, then explicitly confirmed that Muse's database
  permissions are needed. Preserve those permissions and credentials.
- **runtime:** Release `20261121T034350Z-cb9e299-main`, code source `cb9e299`,
  is live with schema19, policy `forge-coding-v10` and matched ABI10. Source agent,
  web host, API, coding and publication services are active and boot-enabled.
  Temporary candidate units are stopped; the existing proxy again forwards to
  permanent port8788. Cloudflare access policy and mTLS credentials are retained.
- **Muse access:** Existing role `muse_events` keeps its13 table SELECT grants,
  schema USAGE, database CONNECT, localhost SCRAM rule, login/password and the
  existing owner/schema/table default SELECT grant for future tables. The exact
  optional read contract is checked without revoking or changing those rights.
  Native Muse and Grok execution stay disabled; email remains Muse's primary
  transport. Unverified external intake publication/approval remains dormant.
- **data and recovery:** A fresh schema17 as-found archive was restored and
  compared across all35 tables, complete row contents, constraint semantics,
  schema/ACL/function/trigger/index/owner state, database grants and sequences.
  Its old strict verifier lacks the authorized Muse read contract; this is
  recorded explicitly. M18/M19 then applied and current strict schema/privilege
  checks passed. A fresh schema19 archive also passed restore/fidelity checks and
  its pinned strict production verifier. Historical rows and old migration
  records were preserved; only the independent GPU seen_at heartbeat was allowed
  beside the new migration records during the upgrade.
- **verification:** Prior full Rust CI and all28 browser groups remain applicable
  to identical compiled Rust and asset bytes. New reader regressions passed7/7
  with no skips, and the integrated reader/policy upgrade run passed27 tests.
  The live release passed asset hashes, real mTLS inspection, session/cookie,
  Host/Origin and CSRF checks. An isolated actual-browser journey through the
  existing local proxy passed Agents readiness, Workbench, Home, Design, legacy
  Chat entry, single-tab navigation and phone overflow checks without page errors.
  Public HTTPS still redirects unauthenticated requests to Cloudflare sign-in;
  an authenticated external Cloudflare browser journey was not performed.
- **source authority:** `refs/forge/verified-main` and its durable completion
  receipts remain unchanged. Runtime deployment and the read-only inspection
  snapshot are separate from that receipt-bound coding-project source base.
  No model task, email, Jenkins build or GitHub workflow was submitted. Repository
  Actions remain disabled per the earlier user stop.
- **evidence:** Private rollout, saved unit/config state, backups, source hashes
  and final receipt are under `/var/lib/forge-provisioning/deploy-main-20261121`;
  safe source/test records are under `/mnt/data/homelab-build/deploy-main-20261121`.
  The later documentation/ancestry checkpoint does not change deployed code.

## Retained source consolidation — 2026-11-21

- **authorization:** The user explicitly requested all feature branches be
  consolidated into main and deleted afterward. This supersedes the earlier
  local-only publication restriction for this source consolidation. The earlier
  stop on GitHub workflows remains in force; repository Actions are disabled.
- **delivery:** Jenkins installation/client/CLI, Grok adapter and portal support,
  reviewed native Muse compatibility, and previously delivered feature histories
  are consolidated. Preserve newer accepted source when old branches contain
  equivalent patches. The three historical private-sender branches and the new
  Muse intake branch use audited sanitized source coverage; their raw histories
  are retained locally in the protected backup and preserved worktrees rather
  than becoming main ancestors.
- **boundaries:** Source supports P10, additive schema19 and matched ABI10.
  Migrations1–18 and frozen historical policy/receipt identities are preserved.
  Native Muse and Grok execution remain default-disabled; Muse email is primary
  and native metadata refresh is explicit. The new external Muse intake is a
  dormant design/validation scaffold: no automatic consumer, database authority,
  service or external completion receipts are activated. Fabricated process-stop
  proofs from its original prototype were removed.
- **live:** This is a manual source consolidation, not an autonomous workflow
  approval or deployment. The portal remains the a3968c1 release with
  schema17/policy8/ABI9. Do not advance verified-main or manufacture database,
  publication or hosted-check receipts. No Jenkins build or new model task was
  submitted. Existing native Jenkins and installed CLI evidence is retained below.
- **verification:** Full fixed `make ci` passed, including debug/release tests,
  Clippy, docs, minimal-feature profiles and deterministic generated assets/WASM.
  All28 actual browser acceptance groups passed with successful cleanup on
  code commit3006529. The expanded browser journey uses a270-second overall
  deadline within its existing300-second supervisor; individual assertions are
  unchanged. The full590-case backend/assets/Studio run had zero skips and one
  installer omission, then all19 final focused cases passed after publishing the
  three missing native modules, including that regression and12 new dormant
  intake cases. Production stores, workers, SQL and compiled Rust inputs are
  byte-identical to their full-gate inputs; only the tested installer fix,
  dormant scaffold, browser harness budget and documentation were added later.
- **preservation:** Keep all linked worktrees, receipt-owned snapshots and dirty
  files. After remote main acknowledgement, detach feature worktrees at their
  same commits and delete only reviewed branch refs using exact remote leases.
  Protected inventory, source audits, test reports, source-equivalence proofs,
  Git bundles and final publication/cleanup receipts live under
  `/mnt/data/homelab-build/merge-all-20261121`; none are runtime authority.
- **next:** Main is the sole branch after verified publication and cleanup.
  Deploying schema19/ABI10 or implementing the external Muse intake requires its
  own appropriately verified activation; do not treat this merge as activation.

## Retained Jenkins implementation checkpoint — 2026-11-21

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
- **progress:** Complete locally. All accepted adapter commits through `a6266cf`
  and CLI commits through `fc86d02` are imported and independently approved.
  Public register/freeze/factory/provider substitution, 18 adapter boundary/native
  HTTP cases, independent Cargo consumer, CLI admission/private-file/FIFO/help
  cases and default/minimal feature profiles passed. Integrated targeted tests,
  Clippy with denied warnings, formatting and the architecture/dependency guard
  passed. New dependency selection does not expand existing default consumers.
- **live_cli:** Runtime Rust source is `8990b1f`; installed command is
  `/home/lrocha/.local/bin/forge`, pointing to its versioned artifact. SHA256
  `64bd0315744b3ccd51079f2e3221e015feed3ba6b3d4774538b5742c3b55b2b7`.
  With the private Mission Control config, status reported Jenkins2.580.2 with
  security enabled and executor count0; jobs reported count0. A build request was
  denied by read-only policy; an absent queue item returned the typed not-found
  error. No build or GitHub workflow was submitted. Source/evidence remain local;
  no remote push or main merge was performed.
- **evidence:** `/mnt/data/homelab-build/forge-jenkins-io-20261121/live-cli-verification.json`
  records the exact artifact and nonsecret outcomes. Token provisioning intent,
  secret recovery receipt and safe report remain under the protected
  `/var/lib/forge-provisioning/jenkins` directory. No token value is committed.

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
