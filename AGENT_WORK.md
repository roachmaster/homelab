# Current: Fix repository review findings — 2026-11-23

User authorizes implementing all five findings from the review of be38906 through 95651a5. Root integrates reviewed changes, qualifies intentional source fingerprints, verifies and publishes. Preserve live services and existing history.

Use the Forge provider entry skill. Keep one writer per worktree and preserve all old receipts, SQL migrations1–26, legacy profile compatibility and unrelated untracked data. Never replace historical success/failure evidence, bypass verification, submit model work or enable GitHub Actions. Record meaningful checks against actual source. Root owns main/publication and any explicit rollout.

Ownership: `jenkins_main_run` owns the isolated CI branch; `latest_source_audit`
owns layer enforcement; `portal_release_ready` owns active documentation. Root
integrates the reviewed commits here and reconciles only intentional source
qualifications before running inexpensive checks and the required full gates.

The CI design retains exact legacy v1 receipts and introduces an explicitly
selected scope-aware v2 contract. Both v2 scopes have three real stages: branch
source/Python qualification, Rust, and in-memory backend; main early qualification
with Rust, browser QA, and all four backend suites with zero skips. No synthetic
browser success, shortened legacy receipt, database migration or historical
policy reset is authorized. Source/profile/scope and gate inputs remain bound.
The installed runtime remains at the separately qualified 95651a5 deployment
until any later rollout is explicitly verified; source changes confer no live
activation claim.

Documentation integration: reviewed223ca465 corrects active guides, ABI13/schema26, current956 deployment provenance and inactive/optional capabilities. Its262 scoped local/repository links and current-state assertions passed. Historical rollout evidence stays separate; remaining deleted-package examples are handled in a scoped follow-up.

Follow-up: replaced all six removed forge-bootstrap-demo commands in src/rust/README.md with actual forge-core/local/terminal targets and feature combinations. Linked real core examples, marked the old493-test/1.99.1 report as a dated18 November record, and corrected removed-demo/current CI wording. Verified manifests, disk/jobs features, forge binary path, both example files and all local links statically; no Rust build or runtime execution.

Layer enforcement integration13301ce: actual source-aware Python graph/lexer guard replaces the weak literal scan; Rust-only contract tests retain no Python dependency.19 bypass/allowed regressions,39 parser/import tests,89 core tests/doctests, Clippy, formatting and source scans passed on the component. No production type/signature changes. Final integrated gates remain pending.

CI integration7745032: explicit legacy/scoped contracts drive closed privileged producer and typed delivery/native consumers. Main runs all four zero-skip backend suites; declared branch tests keep browser/DB not-run.140 focused contract/producer/consumer/bootstrap/history/repair tests passed28.625s, zero outer skips; frozen v1 maps and current policy selection remain explicit.11 intentional raw qualifications follow independent review;26 historical migrations and20 authority ASTs remain unchanged. Installed services/configuration are untouched.

--- Previous task record ---

# Current: Verify latest main in Jenkins and deploy the portal — 2026-11-22

Jenkins16 passed Rust/browser gates and the corrected cache E2E, then exposed
the coupled M27 source fingerprint for that test fixture. This pin was missed
in the fixture fix. Root assigns a one-hash qualification of reviewed `e5a1bd3`,
retaining its historical fingerprint as provenance and every production, SQL and
authority AST guard. Run the complete 24 M27 checks together with the 32 focused
cache/supervision cases before integration. Root also assigns a full ordinary-user
954-test backend preflight in parallel with the next genuine Jenkins execution;
keep failed16 evidence intact and never treat local preflight as Jenkins proof.

The combined 56 checks (all 24 M27 plus the 32 cache/supervision cases) pass in
7.917 seconds with zero skips. Only the reviewed fixture's single hash changes;
the old hash remains documented, all 26 historical migrations and 20 authority
AST pins pass unchanged. Full ordinary-user backend discovery follows this
checkpoint under a separate low-priority user unit, with actual source-bound
logs retained outside Git.

Jenkins15 on `a3a14e9` passed Rust and browser gates, then found one backend
fixture failure in 954 tests. Its delivery-to-supervisor cache test still
replaced only the full gate commands after branch execution became a separate
subset; it consequently ran production unittest discovery in a tiny fixture
repository. Root assigns this isolated worktree the fixture-only correction:
bind its branch cargo/unit subset, assert the exact executed checks, and retain
the full gate identity and real cold/warm/corruption/cleanup checks. Preserve
the failed receipt and production helper/delivery code. New Jenkins execution
and portal deployment remain held until Root integrates the reviewed fix.

The exact failure reproduced locally: the second nested check discovered the
absent production test directory and returned 1, with cleanup confirmed. The
fixture now binds cargo/unit branch commands and their labels while keeping
the full three-command manifest/cache identity. Its three successful runs and
dirty-source rejection all assert the exact two executed checks. All 32 focused
delivery/cache E2E, branch/main selection and supervision-boundary tests pass in
7.323 seconds with zero skips; no production source, helper or cache changed.

Jenkins14 on `422db0f` failed in its first gate: the terminal startup-error
test unconditionally expected disk-enabled diagnostics in a no-default-features
build. Root assigns this same isolated worktree to correct the test's feature
matrix, inspect equivalent Jenkins/jobs options and run focused terminal suites
with all features, no defaults, jobs only and disk only. Preserve the correct
production parser and the original failed Jenkins receipt. Root retains main,
publication, fresh Jenkins execution and deployment ownership.

The corrected matrix retains every disk-enabled diagnostic, verifies unavailable
disk/Jenkins flags before value validation, and exercises the intentional
no-disk idempotence without the disk feature. Jobs duplicate diagnostics remain
feature-independent; existing binary cases verify disabled jobs still allow node
commands. All terminal package tests pass with all features (55), no defaults
(30), jobs only (38), and disk only (39): 162 executed, zero failed or ignored.
Checks used stable Rust, offline/locked dependencies, two build jobs and the
ordinary-user Gemini target. Rust formatting and `git diff --check` pass.

The user authorizes pulling latest Homelab, running Jenkins, fixing failures and
deploying the verified result to the portal. Root owns integration, real Jenkins
execution and live deployment. `latest_source_audit` owns only this isolated
`fix/m27-retirement-context-20261122` checkout from
`370bcffd6d4c71d83ba0fa8742257713da8cf76d`.

Preflight found eleven stale Rust source fingerprints in the M27 retirement
boundary test. Their reviewed changes are the existing module extraction,
`forge-core` import migration and additive tests. Refresh those fingerprints and
clarify their current source qualification while retaining the retirement origin.
Preserve all twenty authority AST guards, migrations 1–26, retired Chat/Mail
behavior, dormant data and ABI13. Production code, policy, SQL and runtime
configuration are unchanged by this fix. Full Jenkins verification and deployment
remain Root-owned; this checkpoint makes no live-delivery claim.

Focused validation: all 24 source-boundary, codec, event/config and normal-host
reviewer/Jenkins composition tests pass in 0.792 seconds, with zero skips. The
nine boundary tests retain migrations 1–26 and all twenty authority AST guards;
all eleven refreshed Rust fingerprints match the reviewed current source.

--- Earlier task checkpoints ---

# Current: Consolidate reviewed Forge features on actual merged main — 2026-11-22

The user explicitly requests committing and merging current reviewed feature changes and cleaning confusing task presentation. Root assigns this isolated task/consolidate-main-20261122 source checkout, based on actual verified cache merge9bf50625c86197e38a3d5fe75a51f8dc29faac09. Main9bf is tree-identical to corrected cache4dc. Import only reviewed feature deltas:4dc→351e (Jenkins M27 plus reviewer-only corrections),351e→11fa (server task scopes and typed generated UI), and351e→2437422 (reviewed Chat/Mail retirement). Do not import unreviewed541 ancestry.

Preserve cache047 and all source/evidence artifacts, migrations1–26 and20 legacy authority guard ASTs, raw task/history data, Owner26 fences and approvals/CI budgets. Resolve only legitimate overlapping Rust UI/session/test code. The sole textual3way conflict was the ABI13 documentation comment; both reviewed features use the same not-yet-deployed ABI13. This composition itself has no live, main/push, database, service, model-task or source-admission effects. Root owns independent review, one genuine consolidated Jenkins verification, main integration, backup/migration and matched runtime/asset activation.

Ownership: gemini_ops composes source/pins and focused checks; cache_recovery_audit independently audits source inventory/preservation. Factory/provider wiring follows the reviewed features. Generated application assets remain Rust/YAML-owned. Reference pins distinguish unchanged baseline bytes from intentionally included task-organization and retirement composition bytes; they are source qualification, not fabricated approval.

Local focused composition checks passed:31 Python/owned PostgreSQL/source-boundary/bootstrap/lifecycle cases in45.050s;67 Agents model cases;16 typed task-scope and inert Chat/Mail snapshot contracts;27 generator cases;authenticated host task-scope validation;retired-capability/config routing and workspace dormant-data round-trip checks. Package formatting and diff checks pass. The independent inventory confirms all103 changed paths belong to the three intended feature deltas, all50 SQL files match reviewed M27 source,20 authority AST guards remain unchanged,12 retired browser files remain absent and no541 ancestry is introduced. Root independently reviewed the source and owns final genuine consolidated Jenkins/browser checks; prior component full-suite/browser evidence remains attributed to its original context, not relabeled as this combined source.

## Current: Explicit source-bound verification recovery — 2026-11-22

- Native build 5 genuinely passed `make ci`, then failed browser acceptance at history pagination; backend checks were not run and cleanup passed. Keep its original receipt and failed review intact. The global fixture fault could be consumed by a background Refresh watermark instead of the intended Next page. An owned forced-order HTTP case proved this mechanism. The corrected task/cursor-scoped fixture passed all 28 actual browser groups with all three cleanup records; production history logic was unchanged.
- Explicit recovery now appends one verification per failed review using the same stopped artifact and original published stop proof. It validates the intervening owner/control lineage and uses a private source-bound authority. Manual counters increase honestly beyond the unchanged automatic allowance; no author, model attempt, Plan reset, automatic budget renewal or migration is created. Original v1 receipts retain exact replay.
- The reviewed source passed 40 focused PostgreSQL/API/compatibility cases, including races, lost acknowledgements, malformed history, immutable source and automatic fencing. Native check accounting passed 74 relevant cases and now retains each protected receipt's actual passed/failed/not-run outcome without allowing invalid hosted success. The integrated recovery, v1 compatibility, native-Jenkins and API suite passed 57 tests in 101.077s with zero skips. Runtime activation and subsequent real verification are recorded in the external recovery checkpoint. Running Rust assets stay unchanged until the matched host/WASM bundle is deployed after genuine verification.
- The earlier main-freeze and deferred-next-M27 integration instructions below describe the first CI run. That run is terminal. The reviewed contract, fixture, accounting and explicit owner-recovery fixes are now integrated for the next real verification of the preserved source. Do not reset old counters or manufacture approval; complete genuine CI, independent review and guarded merge.

## Earlier: Verification-retry response contract — 2026-11-22

Root delegates a source-only Rust contract fix on isolated branch `task/review-retry-ack-20261122`, based exactly on `a1ee6f64ee377a6c3640b86d70de5ece8373aacc`. The backend correctly commits one infrastructure retry and returns a new review UUID; the portable response validator incorrectly requires the old review UUID, causing a gateway failure after success. Accept a distinct valid review UUID with AwaitingReview only for RetryInfrastructure; retain request/project/task/revision/repair-task guards and exact target identity for all other decisions. Add protocol, real host-consumer and generated-application browser-fixture regressions. This is pure contract behavior with no new factory, API field or ABI version.

Do not change main, push, merge or deploy while genuine cache Jenkins CI runs against a1ee. Keep all production HTML/CSS/JS generated from existing Rust/YAML. Use the existing ordinary-user warm Cargo cache with the installed stable toolchain and focused checks; no protected Jenkins cache or cold workspace rebuild. Root will incorporate the local feature context into the next normal M27 Plan, with genuine full CI/review afterward.

Verification: all 205 portable UI tests and 21 authenticated host work-consumer cases pass; all three owned browser-relay fixture tests, JavaScript syntax and workspace Rust formatting pass. The fixture returns a distinct review UUID, preserves the finished author/profile/round, replays one intent once, and exposes the replacement review through WorkGet. The real browser journey requires that ACK to clear the pending action and refresh the new review without another author submission. The fixed full browser suite passed all 28 groups on clean 503d26a in 135.973s and again on clean 397f711 in 152.695s, with zero failed/not-run groups and all three cleanup records passed. The 33 QA-harness checks also pass. These are local qualification, not a native Jenkins source approval. Stable Rust 1.97.1 rebuilt the actual release WASM; ABI/runtime, generated worker and input-presentation checks pass. Two fresh five-asset generations are byte-identical, including their digest manifests, under the external review-retry-evidence directory. Production assets in Git and installed assets remain untouched.

Deployment requires rebuilding the host binary and matching browser WASM/assets from the same final Rust source. The shared response validator changes in both consumers; the wire fields and ABI12 version remain unchanged. Generate assets through forge-ui-assets/ui-build and retain the matching digest manifest; do not hand-edit production JavaScript, HTML or CSS. No live deployment or claim of full CI is made by this local patch.

Final scoped lint check: stable cargo clippy for forge-ide-ui and forge-ide-web-host, all targets, offline/locked with warnings denied, passes. Builds used CARGO_BUILD_JOBS=2 and the existing external gemini-target only. The initial default-toolchain attempt failed on pre-existing Rust 1.86 incompatibility; the successful checks use the already-installed stable toolchain, with no toolchain installation or source compatibility change.

Follow-up browser history investigation: genuine Jenkins build5 failed an existing Next-events assertion. The global test fault could be consumed by Refresh's unseen-events read at cursor61 before the intended page read at cursor50. An owned loopback reproduction forced that ordering and proved watermark61 received503 while Next50 succeeded with events51–61. The natural browser failure did not recur in the first full run. Scope the one-shot fault to the intended task/cursor, observe exact request identities, wait for the real Refresh response and rendered failure, then preserve every existing disclosure, retained-page, retry, count and disabled-state assertion. A regression proves other-task and watermark reads cannot consume the fault, followed by one denied page and a successful retry of the same cursor. No production history logic changed.

External evidence is under /mnt/data/homelab-build/cleanup-conflicts-20261121/review-retry-evidence: legacy-history-race.json, browser-runs/forge-ui-qa-3l9a_nui and browser-runs/forge-ui-qa-mr9uycp_. The final clean browser source is 397f711d2c29cabc3d00043c88f69af7eb5774a8 with source fingerprint 832a2a40295e7b18e4460845ffdba4f381ad13e47ba961d01ffabace5182d591. The subsequent checkpoint changes this file only; production and test bytes remain identical to that qualified source.

## Earlier: Recover native publication ancestry — 2026-11-22

- The continued cache author finished normally with the corrected cache helper and measured qualification report. Native Jenkins build4 then rejected its old-base candidate at `main_ancestor_required` before running checks. The finished native workspace and all failed evidence are preserved; the idle publisher was stopped pending the controller fix.
- Trusted branch-first preparation now snapshots authored source in its private clone and three-way merges actual current main before branch push, the durable event and Jenkins. Version2 receipts freeze both authored and integrated identities; historical receipts replay unchanged. Conflicts produce unapproved source findings before any push or build.
- One explicit owner verification-only command can append a fresh local review of the same stopped author result after this failure. It preserves the failed review, original owner fence, Plan and spent repair history; it starts no model writer and renews no automatic allowance. Fresh CI, independent review and guarded merge remain required.
- Publication changes passed 68 focused tests and independent review. Owner recovery passed 56 affected cases and an additional native acquisition assertion. The integrated owner/publication/review/native-Jenkins/API suite passed 70 tests in 58.476s with zero skips. Final compatibility review added legacy empty-note fallthrough coverage; all 8 owner verification cases passed in 38.400s. Verification and the real delivery outcome are recorded outside the source tree under the existing cleanup evidence directories. Source-only API/publisher activation is pending; schema26, runtime configuration and Rust/ABI12 assets remain unchanged.

## Earlier: Clean up stalled pipeline work — 2026-11-21

- **authorization:** User requests cleaning up and fixing the stalled conflict workflows. Preserve saved source, immutable Plans/verdicts, budget counters and actual source authority. Never manufacture acceptance or reset an exhausted automatic budget.
- **coordinator fix:** A genuine Code Review Revise at the frozen2/2 stagnation limit caused SQL P0009 to escape the inner savepoint, rolling back the verdict and retrying forever. The deployed API hotfix now records the exact verdict/reviewer result and explicit exhausted-budget handoff atomically. Scoped64 real PostgreSQL/feedback/replay/preparation tests and independent review passed. Schema25, credentials, policy snapshots and all other services were preserved at activation.
- **owner continuation:** The existing authenticated WorkResume route admits one explicit source-bound owner continuation after that genuine handoff. Bootstrap injects a ready native owner-control port. Preserve the same task, original Plan/recipe/repair lineage, stopped source and spent budgets. Require a fresh actual reconciliation receipt, current scope/readiness and immutable peer/source evidence. Durable idempotent authority fences every subsequent automatic writer, including merge/source failures and real-progress stagnation resets; independent approval, full CI and trusted merge remain mandatory. No automatic scanner can invoke this owner action.
- **database boundary:** Additive M26 coding_owner_continuation adds no tables or roles and no direct worker table-read grant. Exact function/owner/ACL verification and role-local capability wrappers fence both API and independent SQL admissions. M1–25 bytes and historical credentials, grants, default privileges, HBA and task evidence are preserved. A fresh strict schema25 backup and owned restore verified all45 tables before any proposed live M26 application.
- **verification:** Independent review approved integrated265d5ae production source. SQL qualification passed93 actual PostgreSQL tests; owner admission passed11 cases plus2 strengthened real-progress fence cases, API20 and legacy Resume1. Integrated full-backend discovery passed775/775 in680.715s with zero skips. No source approval is inferred from operator qualification.
- **actual cache evidence:** Native Jenkins build3 passed all3 fixed gates, all769 backend tests and confirmed cleanup on a19a46a. This was not source approval: independent Codex found swallowed cancellation, orphaned snapshots escaping retention, and missing live cold/warm qualification. Corrected source a663838 passed36 tests under umask077; combined qualification d4ea074 passed47 targeted regressions and independent review. An isolated operator Jenkins job is measuring cold/warm/clear behavior with real receipts. Qualification is separate from the task's future author, CI and reviewer identities; it cannot approve or merge source.
- **cleanup:** The obsolete plan-only c6aa3fde task was normally cancelled as superseded (revision7), retaining its artifact and review history. The distinct old Jenkins-events task remains intact. Selective unreviewed reference307d034 passed100 focused tests; its never-applied migration must become M27 after owner-continuation M26. Do not overwrite a shipped migration or substitute prepared source for a new current-policy Plan/review.
- **deployment:** Owner M26/API activation and actual same-task owner continuation remain pending. Keep API environment/configuration and Rust/ABI12 assets unchanged. Preserve the external job and old immutable spool/binding/result receipts. Record actual activation, qualification and subsequent workflow outcomes privately.
- **evidence:** /mnt/data/homelab-build/cleanup-conflicts-20261121 and /var/lib/forge-provisioning/cleanup-conflicts-20261121.

# Shared Agent Work Checkpoint

## Current: Push, build, merge and return failures to Gemini — 2026-11-21

- **authorization:** User specifies the actual delivery loop: Gemini's code is
  pushed, an event starts Jenkins, successful CI proceeds to merge, and failures
  return an event/build reference to Gemini for CLI investigation and repair.
  Keep independent code review and the existing bounded budgets. Normal source
  conflicts must not require a human to manage the integration manually.
- **ownership:** Separate worktrees own native branch/CI/merge providers, M25
  SQL/DAO/event transitions, and the separate native Jenkins job/operator staging.
  Root owns configuration, the attempt-scoped build inspection CLI, integration,
  documentation and live qualification. Preserve receipt-bound task workspaces.
- **implementation:** Explicit `branch-first-v1` publisher selection pushes the
  exact owned source, records the idempotent database event and requires its
  receipt before Jenkins. A separate native-only job checks expected-main
  ancestry before the full unchanged gates. Build or confirmed merge failure
  produces bounded repair; genuine Git conflict results become explicitly
  unapproved source with current-main files and markers preserved. Fresh source
  must pass Jenkins and independent review again. Unknown write acknowledgements
  remain read-only reconciliation, without a duplicate push or new writer.
- **agent I/O:** Explicit `build_results_root` injects a worker-owned reader.
  The current lease/feedback binds the source review, actual action/job/build,
  private sidecar and root receipt/log hashes. `build_info.py status|logs` supplies
  sanitized paginated evidence; no large log is inserted into the prompt. Raw
  CI results are masked. Dependencies are created/injected at bootstrap.
- **history:** M25 adds four narrow functions and no tables/roles. Preserve M1–24,
  P10 and AGY policy identities, old source receipts, Plan/code counters and
  immutable approvals. A typed once-only replay admits the actual stopped
  pre-CI conflict after deployment; it consumes the existing infrastructure
  allowance and preserves the original failed receipt. No raw status reset.
- **verification:** SQL/native35 final contracts and99 broader regressions,
  independently reviewed backend77 tests, Jenkins19 tests and build-CLI/worker49
  tests passed. Full integrated discovery passed749/749 in491.293s; the final
  28-test loop/context/PostgreSQL run passed after the additional inherited-code
  regression. Seven offline ops checks passed. A real inherited-feature regression confirms
  review compares against actual main rather than the unapproved repair seed.
- **deployment:** Pending final source publication and strict25 restore proof.
  Fresh strict24 backup/owned restore run `8cc36dc2c1e9` verified45 tables.
  Stage only API/publisher/Gemini source; API env/config stays byte-identical,
  Gemini adds the build root and publisher selects the separate native job.
  Preserve the external job's5b28 verifier and token/config identities. Record
  activation and real CI/repair evidence privately after the final main push,
  avoiding a documentation push that would invalidate a frozen candidate.
- **evidence:** `/mnt/data/homelab-build/push-build-merge-20261121`.

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
