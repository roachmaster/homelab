# Current: Qualify Grok feature-branch runtime and publication — 2026-11-27

Completed runtime correction47789e90, published on feature/codex and deployed.
API, Grok worker and publisher explicitly resolve grok-primary to feature/grok;
their original workflow policy snapshot is byte-equivalent. Legacy defaults and
historical frozen task/branch/CI receipts are preserved. Other agent profiles were
not silently reassigned. Main remains untouched; future integration retains CI.

Native feature validation is scoped to each known agent profile; wrong-agent
feature refs, main, malformed refs and ephemeral task refs remain rejected.
New feature-assigned tasks get independent private bare repositories under the
canonical Git directory, with no shared objects/alternates. Read-only Plan/Review
keeps its pinned detached snapshot; implementation attaches the exact owned
feature branch. Plan-to-implementation retains metadata/.git bytes/resume digest;
stopped dirty workspaces survive and never block or reset another task's branch.
Canonical cache/browser provenance and exact common-Git-directory, branch/head
and symlink checks remain enforced. No existing workspace is silently migrated.

Additive migration0051_agent_feature_branches changes only the coding_publications
branch constraint and two publication branch checks. It preserves all other scope,
profile, epoch, candidate, token, audit and idempotency checks. Strict source/owner/
language/return/ACL verification precedes exact predecessor normalization. Existing
migrations and the deferred destructive digest-column drop remain untouched.

Validation passed:50 branch/executor/cache/SQL tests;52 upgrade/owner/reader tests;
34 Jenkins source-boundary tests after adding the new verifier to its closed
inventory; native delivery/content identity tests; and real Git-only systemd
sandbox proofs, including the actual /home/lrocha/repos/homelab source path.
Those probes saw feature/grok, read Git status and wrote the owned workspace,
without invoking a model or changing the canonical checkout. Independent Python
and SQL reviews found no outstanding blockers.

A root-private physical schema50 copy passed strict before/after verification,
50-to51 upgrade and idempotent reapply. All task/event/workflow/control-receipt
row counts and full-row fingerprints matched before/after. Logical pg_dump
rehearsals exposed representation differences in explicit-owner ACLs and CHECK
parse trees; no production verifier was relaxed to accommodate those artifacts.
The live upgrade also preserved every task/event/workflow/control receipt, and
setup-postgres.sh --check passed. No grants, credentials or model settings changed.
Root-private backup/rehearsal/configuration/activation receipts:
`/opt/homelab-forge/operations/grok-feature-runtime-20261127/`.
Runtime source: `/opt/homelab-forge/releases/grok-feature-47789e90d93c/source`.

Execution hold released after matched qualification. Grok claimed97711535 as
attempt2b8b1532-a09c-45d4-9db4-43c9a0f7229f at2026-11-27T03:49:57.722964Z;
the task is running its read-only Plan step, with Codex independent review next.
Implementation/publication will use feature/grok. All nine services are active.
Shared repo/landing AGENTS notes explain that the older prompt's pending-hold
statement is superseded and deployed schema51 must be accounted for explicitly
before any new migration. This does not revive earlier operator-cancelled work.

# Previous checkpoint: Muse proxy task reassigned to Grok — 2026-11-27

User explicitly requested replacing Muse with Grok or AGY. Selected Grok, the
existing native default, without another project-policy change. New task
97711535-4007-4bec-b995-67d90c560ead / workflow e1683a99-9d9e-4155-88bc-8129840301a2
is queued for grok-primary with codex-primary independent review. Its prompt
links the original request/acceptance, prior plan artifact and read-only Muse
source, requires a fresh reviewed plan and feature/grok-only edits/publication.
Only after verifying this successor, cancelled original Muse workflow b3a05d8a
through the normal operator API, retaining every task/artifact/review/event.
Original task6fdff749 remains historical, not falsely completed.

Execution is held: native Grok config resolves forge/agent/grok-primary; Python
native profile/branch validation and SQL native publication receipt validation
still require forge/*, so merely changing a prompt/config cannot qualify
feature/grok. Grok worker was idle and stopped before admission. No attempt or
branch reservation exists for the new task. Do not start it before an explicitly
qualified branch-policy transition; preserve all other workflow identities.
An explicit root-owned systemd condition preserves this hold across reboot:
`/etc/systemd/system/forge-grok-worker.service.d/~owned-branch-hold.conf`, gated by
`/opt/homelab-forge/operations/muse-to-grok-20261127/execution.hold`.
Remove the hold only after feature/grok admission, execution and publication are
qualified. The API and all other worker/portal/MCP services remain running.
Evidence: `/opt/homelab-forge/operations/muse-to-grok-20261127/`.

Admission exposed the same strict response/audit mismatch as cancellation:
WorkStore.submit durably records actor, but Rust WorkSubmitted has no actor field.
The first request committed one task then portal returned503. Never submit a new
key on that response; verify/replay the original key. Root is applying the same
public response projection while retaining the full audited admission receipt.
Correction a1b0d44d is now published and live in the API-only release
`/opt/homelab-forge/releases/admission-response-a1b0d44d370b/source`. All27 API
tests passed; independent review found no blockers. Two authenticated same-key
live submission replays returned202 and the same task UUID, without creating
another task or starting an attempt. Queue counts are active1, attention13,
history48, all62. No global project defaults or frozen identities were changed.

# Previous checkpoint: Root-cause analysis and contract-name handoff correction — 2026-11-27

Code/history/production-evidence review confirms two separate completion blockers.
No CPU or hashing-performance claim is made: the evidence establishes control-flow
rejection, repeated work and disabled workflow admission.

## Root cause 1: Contract-name migration missed the preparation consumer

The v2/v3 Jenkins producer emits `<profile>:<branch_scope>` from
work_external_delivery.jenkins_gate_manifest. Jenkins and the release actor accept
that named contract. Protected successful main receipt b5727985-b846-4ed4-a65a-e969b6ee1623
verified a3ec03b8 with forge-sdlc-jenkins-v3:main and cleanup confirmed.

But ExternalWorkflowStore._step still grouped gate_manifest_sha256 with actual
diff/source hashes and required all three to be64hex. An effect-free reproduction
through the actual installed predecessor queued review with the legacy gate and
raised 'Candidate lacks exact gate/diff digests' for the corresponding named gate.
run_once catches that ValueError, rolls back the step, then escalates the workflow
with the generic 'Workflow input could not be verified: ValueError' reason.
This is an incomplete protocol migration, enabled by duplicate gate validators
and a legacy field name that no longer describes the gate's value type.

Correction b541ad5f, published on feature/codex: producer/consumer share a pure
closed gate-reference validator in work_ci_contract. The consumer accepts named
contracts or historical64hex receipts, binds names to the exact prepared profile
and branch scope, and retains strict diff/source hashes, commit/tree/base checks,
independent review and content-v2 generation checks. No privileged verifier or
actor pins, SQL grants, schema, task identities or audit records were weakened.

Validation:20 contract/handoff tests and36 external-flow/content-identity/main-refresh
integration tests passed with no skips; an additional real disposable-PostgreSQL
named-gate content-v2 handoff test passed. Independent read-only review found no
blocking findings. The root-sealed source's eight handoff tests also passed as
forge_agent before API activation. Runtime evidence and deployment state:
`/opt/homelab-forge/operations/workflow-gate-20261127/state.json`.
API source: `/opt/homelab-forge/releases/workflow-gate-b541ad5fa163/source`.
Other workers retain their compatible named-gate producer release. Main has not
been pushed or merged; future main integration retains its required CI.

Important limit: live Muse workflow b3a05d8a is repair, active task b178c144 is
queued, external assignment generation0, with no returned artifact/prepared
candidate. The mismatch was a confirmed next-preparation blocker; it is not an
observed explanation for this currently unclaimed assignment. MCP SQL/heartbeat
access alone supplies no task-claim or push-notification tool. Check whether the
designated external author polls its approved assignment API before retrying it.

## Root cause 2: Default-agent changes invalidate preserved workflow authority

Actual database records show15 workflows with frozen AGY author policy25ce3a1e,
Codex reviewer, and completed AGY-to-codex-author handoffs at
2026-11-26T02:50:13.170523Z through02:50:14.561583Z. At03:15:34.768146Z through
03:15:34.880456Z, the coordinator emitted14 cancellations (12roots and2children)
plus one cancellation request for running root21f9d7d6. Its worker recorded final
cancellation at03:15:50.590580Z. All15 workflow reasons are exactly 'Workflow
delegation or role grant is no longer current.' A later03:33:21.563043Z workflow
bbd6bab5 freezes the Grok default policy75856ff2, retaining the same Codex reviewer.

The historical selected0070e78f coordinator compares the newly configured project
snapshot digest with each workflow's frozen snapshot, then permits only budget
changes through budget_compatible_snapshot. Actual-snapshot comparison returns
False: author profile and author Plan/Implement recipe identities differ. The
coordinator rejects before execution_policy.effective can consider valid handoff
authority. Its catch routes every such Rejected to _escalate, which disables the
workflow, cancels queued children, requests running cancellation, hands off pending
actions and resolves retry scopes. No model invocation or exhausted retry budget
is necessary to explain this synchronized batch.

Current work_store.policy_drifted can compare catalog documents without their
policy_digest field. Those15 legacy workflows have no workflow-policy identity
attachments, so they still use the legacy comparison. The historical0070 source
had no identity-catalog fence at all. Removing hashes cannot fix the semantic
coupling: comparing these documents directly still observes changed defaults.
Increasing retry limits and invoking audited revival alone also cannot fix it:
revival calls _grant, which calls the same _current_policy and refuses grant_not_covered.

Required correction remains explicit versioned compatibility for frozen workflow
authority, checked with current grants and effective handoff authority; typed
deferral for compatible/transient configuration changes; distinct revocation and
operator-cancellation semantics; and an exact incident-bound recovery manifest.
Seven of the original15 workflows were subsequently retired by the operator and
must stay cancelled. Only the remaining eight are incident-recovery candidates,
subject to their preserved source/attempt/plan/receipt and readiness checks.
No workflow recovery, counter reset, snapshot rewrite or new model run was done.

## Present queue and related symptoms

The13 attention items comprise eight policy-escalated workflows, two workflows
stopped by real agent failures, and three obsolete standalone diagnostics. The
two stop summaries are 'The coding agent returned malformed Grok machine output'
(bbd6bab5) and 'Agent invocation did not complete successfully' (42fcaf63).
Neither can honestly be attributed to a gate mismatch from this evidence.
Workflow phase takes precedence in the attention classifier, so seven root tasks
can already be cancelled while their workflow remains escalated/attention.

The earlier30-minute timeout was a separate Antigravity CLI wall bound and is
fixed at the4-hour ceiling; progress-aware lifecycle remains the wall authority.
Seven focused retry-policy/CLI-bound tests passed during this review. Review
infrastructure retry handling also has a separate partial fix and acceptance
work, as recorded below. Immutable Git/artifact/context/receipt digests and
privileged executable pins remain necessary; the destructive old-column drop
continues to be deferred. Runtime source selections were checked against reviewed
modules. No unrelated service/configuration or historical evidence was modified.

# Previous checkpoint: Cancellation response fixed; remaining attention review — 2026-11-27

The cancellation response fix is published on feature/codex as80e688e0 and live
in the API runner. The version1 response now projects only its five declared
receipt fields, matching Rust WorkCancelRecorded's strict schema. Operator actor
metadata remains in durable control receipts and events. Native/external provider
selection, cancellation semantics and idempotency keys are unchanged.

Independent read-only review found no blocking issues. All37 focused API,
retry-transport and progress tests passed with no skips; the new regression
reproduced both native/external failures against the deployed predecessor.
Two authenticated live portal replays of an existing cancellation returned200
with the exact response schema; task state remained identical and all eight
original operator audit receipts retained their actor. No new work was cancelled.

Only production module work_api.py differs from the previous API release. The
runner was restarted after confirming zero active attempt/review/action leases;
other workers, portal assets/actor, database, MCP and configurations were not
changed. All nine services are active. The first startup check raced API readiness
and rolled back; the successful retry waited for work_get before replaying.
Root-private deployment and acceptance evidence:
`/opt/homelab-forge/operations/cancel-response-20261127/`.
Runtime source: `/opt/homelab-forge/releases/cancel-response-80e688e0c9cb/source`.
This is a reviewed, focused feature/codex API hotfix; main remains a3ec03b8 and
has not been pushed or merged. Future main integration keeps its required CI.

## Disposition of all13 remaining attention items

Read-only source/history review below does not assert successful completion of
old tasks or authorize replay of frozen work. Counts remain active1, attention13,
history47, all61. Existing cancellation/escalation history is preserved.

| Task prefix | Finding | Next action / order |
|---|---|---|
| bbd6bab5 | Policy rollover/recovery remains incomplete. work_workflow._current_policy still rejects default-author changes; compatibility covers retry-budget drift only. Audited revival exists but does not satisfy the exact incident recovery request. | First: versioned policy compatibility, safe deferral and source-bound recovery manifest. Never revive operator-cancelled workflows or reset retry budgets. |
| 1d4fe821 | Branch notes are updated, but runtime defaults remain forge/agent/<profile>; project/profile overrides already exist. The old request is partly superseded by the user's newer five feature-branch mapping. | First: align admission/publication and selected profile config with that mapping; qualify new work and preserve historical frozen identities. Stop mismatched jobs before editing/pushing. |
| 92591001 | Review-infrastructure protection substantially landed in888ff6a9; candidate/sibling preservation and audited recovery exist. Full outage classification/backoff acceptance remains unproven. | Next: qualify context-load, auth/quota/permission and retry-exhaustion behavior; implement only remaining gaps. Do not restart the old conflicting candidate automatically. |
| 42fcaf63 | The requested agent-facing base-refresh API is absent. Trusted integration already preserves completed author work when main advances and repeats independent review/exact CI. | Next: reproduce residual redundant rebase rounds against current integration before adding another privileged API. Actual conflicts must retain the existing rebase path. |
| bc54723f | forge-m27-update.py was not found in current tracked source or matching available history; saved notes place the tool in the Muse proxy candidate. | Locate the actual tool/candidate, then reproduce and test first pin, replacement, chained maps, formatting and idempotency. Preserve historical qualification maps. |
| 21f9d7d6 | M46 implements bounded failure causes/summaries and event/portal-history projection. Complete producer coverage and literal list/get acceptance are not established. | Audit remaining producers and list/get visibility; close only after that narrow acceptance check. Do not rewrite historical events. |
| 29cbba02 | CI correlation, stage/log storage, API projection and typed portal rendering are integrated. | Acceptance/retirement candidate: verify one source-correlated run in the current portal, including stages/logs and missing-evidence behavior. Avoid a duplicate implementation. |
| 0e8dcdc6 | M45 typed renew outcomes and worker handling distinguish cancellation, expiry, deadline and missing tasks; test_typed_renew.py covers the distinction. | Acceptance/retirement candidate: verify the current cancellation versus lease-expiry outcome; no new implementation indicated. |
| 9a6ca050 | Jenkins-to-Drive snapshot refresh remains a saved request, not an implemented stage. | Feature backlog: exact verified-main archive, main-only post-success stage, qualified rclone upload/verification, then safe old-snapshot retirement. Keep credentials out of source. |
| dcb9d2e1 | Structured defect submission/schema/filter/badge and triage workflow remain unimplemented. | Feature backlog: backward-compatible defect contract, normal plan/review lifecycle, schema verification and portal acceptance. |
| 666c25a9 | Obsolete successful-output Grok connectivity report, awaiting_review. | Add a supported audited report-only closure/archive operation; preserve its real outcome. No new agent run or fabricated code approval. |
| 0d116b62 | Obsolete failed Grok connectivity diagnostic. | Same audited archive disposition; preserve failure evidence. |
| c8ec1815 | Obsolete failed Grok connectivity qualification. | Same audited archive disposition; preserve failure evidence. |

Relevant source entry points: work_workflow.py:174/_current_policy and audited
revival, work_retry_policy.py/budget_compatible_snapshot, work_agent_branch.py
branch resolution, work_external_workflow.py infrastructure recovery/integration,
work_failure_cause.py and work_events.py, work_store.py correlation projection,
work_worker.py typed renewal, and the saved resubmission-queue feature/defect notes.
The active Muse proxy workflow is separate from these13 and remains untouched.

# Historical checkpoint: Event stream and obsolete task retirement — 2026-11-27

Reviewed the 21 attention items and the active Muse proxy workflow through the
authenticated portal Work API, including task details and event histories.
Retired eight obsolete native workflows using revision-bound, idempotent operator
cancellation: five historical main-verification repairs superseded by deployed
main a3ec03b8 / protected Jenkins104, the integrated progress-aware timeout fix,
the superseded cancel intent_digest investigation, and the historical35-branch
cleanup (the remote now contains main and the five assigned feature branches).
These are cancellations, not newly asserted successful task completions.

Verified scope counts: active1, attention13, history47, all61 (previously1/21/39/61).
Preserved every event ID in the22 reviewed task snapshots; no history, task,
workspace or branch was deleted. All eight durable cancellation receipts and
workflow phases were checked. Private before/after snapshots and rationales:
`/var/backups/homelab-forge/task-retirement-20261127-i6wtt_4h/`.

Follow-up defect: Python cancellation receipts include actor metadata while the
strict Rust WorkCancelRecorded response lacks that field. The portal returns503
after the cancellation commits. Never infer failure from that response alone or
retry with a fresh key; reread workflow state and preserve the idempotency key.
No response-contract fix or deployment was performed during this cleanup.

Retained unresolved feature/defect/policy work and the active Muse proxy workflow.
Three obsolete standalone Grok connectivity checks remain visible: their failed
or awaiting_review states cannot use current manual cancellation (queued/running
only). They need a supported archive/report-only closure, not direct SQL edits
or fabricated acceptance. Runtime agent branch-default migration also remains
pending; preserve the owned feature-branch policy below for all new development.

# Current: Record required agent feature branches in shared notes — 2026-11-27

The user explicitly requires all agents to use only their own named feature
branch. Codex is the primary orchestrator on feature/codex; Dot is a separate
ChatGPT agent on feature/dot. Gemini/Antigravity uses feature/agy, Grok uses
feature/grok, and Muse uses feature/muse. These exact remote branches were
confirmed with git branch -a and a fresh remote fetch. See AGENTS.md for the
mandatory before-edit check, publication boundaries and receipt preservation.

This notes-only change is developed and published from feature/codex, synchronized
with origin/main a3ec03b8. Preserve all other branch refs and worktrees. Do not
push this update directly to main or other agents' branches. Shared Mission Control
notes and the existing Gemini Drive connection guide carry the same ownership
rules so local sessions and external MCP agents can pick them up before work.
Runtime branch defaults are not changed by this documentation update. Automated
jobs pinned to older forge/agent/* branches must be reported for a separately
reviewed, identity-preserving transition before those agents edit or publish.

# Current: Authenticated MCP heartbeat and portal connection presence — 2026-11-27

User authorizes adding an authenticated heartbeat MCP tool and connecting it to
portal agent presence, including separate Dot (ChatGPT), Gemini, Grok and Muse.
Codex is the primary orchestrator, distinct from Dot and native worker profiles.
Keep SQL tools read-only and all PostgreSQL grants/schema50 unchanged. Store only
presence metadata outside PostgreSQL in service-owned SQLite. Bind identities to
administrator-configured authenticated static-token/OAuth-client subjects; a caller
must not supply another agent identity, label, server timestamp or execution role.
Heartbeat connection presence must not advertise execution readiness or job modes.

Root coordinates integration, publication and matched MCP/portal deployment.
Separate worktrees own MCP/auth/presence and Rust typed portal/host/UI. Use existing
Forge provider/UI skills, maintain effects at composition roots, preserve tokens,
OAuth stores/clients/passwords, private configs, task/event histories and worker
roles. The live ad20beed Gemini compatibility fix is retained as09d2b0a4 before new
work. Main wasab65003d with Jenkins103-qualified running ABI16/schema50 portal.
Do not apply the deferred destructive digest-column migration. Keep three obsolete
release retry deferrals and their uncertainty intact. Current heartbeat agents
should appear only after actual calls; never simulate continuous live heartbeats.

## MCP backend implementation checkpoint — 2026-11-27

Isolated worktree `/mnt/data/homelab-build/mcp-heartbeat-server-20261127` owns
only MCP authentication/presence/backend tests and its README/example config.
Stable authenticated subjects are `static:<token_id>` and `oauth:<client_id>`;
existing token/grant IDs and read-only SQL behavior remain intact. Optional
presence factories are selected at `build_app`, then open a separate private
SQLite store and dedicated local-reader credential. No background beat exists.
The local reader returns version1/at most16 configured agents with server UTC
last-seen timestamps; portal host integration and all live configuration/deploy
remain root-owned. Initial existing MCP qualification passed56 tests; the final
combined suite including spoofing, restart/freshness, OAuth-client identity,
reader isolation and the bounded store is pending at this checkpoint.

## Integrated validation and rollout checkpoint — 2026-11-27

The MCP backend passed70 tests (56 retained +14 presence), with no skips or
failures. Portal native presence/clock tests,35 generation tests and make ui-check
passed. A real isolated sandboxed browser fixture at1440 and390 pixels verified
four MCP clients, all three states, exact timestamps, unchanged worker readiness,
no phone overflow/runtime errors and successful cleanup. Root inspected both
screenshots. Root actor/recovery/startup tests passed39, including ABI17 and a
presence overlay that preserves the predecessor configuration for rollback.

The host owns the optional reader and clears any runner-provided presence. Its
read is bounded to8KiB/300ms; failure renders Unavailable without changing coding
readiness. An active-only60-second typed catalog refresh updates presence without
changing project/task selection or usage, and late replies after close are ignored.
Normal PostgreSQL/runtime worker APIs and all SQL files remain unchanged. The
full integrated main Jenkins gate and real deployment/production presence proof
remain required. A temporary clearly named verification identity may exercise the
live tool, then is revoked/removed; never generate fake production-client beats.

# Current: Match portal host admission to generated UI ABI16 — 2026-11-26

The user authorizes fixing verified deployment blockers. Jenkins100 and a clean
local full browser gate on source908fbab1 fail at the first owned Agents fixture:
Assets::load still requires ABI15 while the actual generator/WASM use ABI16.
Make host admission strict to ABI16, retain all manifest/file integrity checks,
and update native synthetic fixtures to the current ABI. Cover stale15, future17
and malformed ABI rejection before remote effects, plus actual generated assets
through the fixed full browser gate. Use pinned existing CI toolchain/browser
prerequisites without installation/security changes or fixture credential logs.
Root owns publication/deployment review; source work stays in this isolated908
worktree with Cargo and QA evidence outside tracked source paths.

The first fixed full browser run passed fixture startup and reached accepted-task
reload. Its stale locator searched the inbox for the reader's full long title,
although triage intentionally compacts inbox titles. Reopen by the fixture's
unique visible prefix and assert exact full-title restoration in the reader.
The failed run confirmed owned-resource cleanup; later groups remained not run.
A subsequent stale assertion still required two-line CSS clipping, which triage
intentionally replaced with whole-word bounded prose. The browser now verifies
its exact bounded prefix, word boundary and no vertical clipping at all three
viewports, while retaining horizontal-overflow and touch-target checks. Reload
also verifies a single matching row and unchanged full title/correlation.
The next browser obstruction was a missing explicit Tests-view fixture record:
the journey seeded only Console, so its legitimate Tests request received404.
Seed the structured test summary/cases for that view without changing application
behavior or weakening the inspector's text, paging or escaping assertions.
The full browser run then reached the expanded implementation reader, whose
acceptance still expected three tables and earlier checkpoint/navigation wording.
Its source document now contains six tables. Keep current source ABI references
at16 in both current-contract rows; dated historical release identities remain
unchanged. Align the reader assertions to the reviewed document and retain all
same-tab, table, safe-link and narrow-layout checks.
The expanded reader's Mermaid text fence caused real phone overflow:375px
viewport,631px document,615px unwrapped code. A scoped declarative pre-wrap /
overflow-wrap rule keeps implementation code readable within the page. Preserve
the existing browser overflow check and inspect owned before/after geometry and
phone screenshots; do not replace the check with a CSS-string assertion.

Validation: all six affected native asset tests and five public asset consumer
checks passed, as did workspace formatting. The fixed full `make ui-qa` passed
28/28 ordered groups with all owned cleanup passed on clean checkpoint
`48dd58f70e0bdc1941270da9f6df5b1c8bb5651e`, unchanged source fingerprint
`030ad6fa51f70b79d48ca6d6a70e65f5bab6437aacbc78b8403e6a9b704bdb8b`.
Evidence: `/mnt/data/homelab-build/portal-browser-fix-908-qa/forge-ui-qa-r2gjr1p0`.
Pinned existing CI Rust1.97.1, browser dependencies and sandbox were preserved.
Owned after-fix geometry matches375/390/1440px viewports; the375px code screenshot
is readable and contained. Final follow-up changes only documentation: neutral
responsibility labels, explicitly dated pre-deployment observations, and the
schema50 source target. Tested application bytes remain unchanged. Exact final
published-source Jenkins qualification and deployment remain root-owned work.

# Previous: Restore bounded embedded documentation rendering — 2026-11-26

The user authorizes fixing deployment blockers. Jenkins99 on main a55a17f8
failed while building the web host: the 38 KiB implementation document uses
an adapter whose selected reply renderer permits only 32 KiB of HTML output.
Expose and reuse the existing bounded document/file renderer for trusted
embedded documentation. Preserve reply limits, HTML escaping, external-only
untrusted links, file input/output/work limits and fixed local reader routes.
Work in an isolated a55 worktree; root reviews publication/deployment separately.
Check the actual embedded document, external library consumer, sanitizer/bounds,
formatting and host build with Cargo output outside the source tree.

Validation on code checkpoint41d05701: workspace formatting passed; all156 UI
library tests, two public renderer consumer tests, six document adapter tests,
and the public host fixed-document HTTP consumer test passed. The actual host
all-features build passed offline/locked. These checks reuse
`/mnt/data/homelab-build/task-consolidation-target`; full exact-source Jenkins
qualification and deployment remain root-owned follow-up work.

# Current: Qualify and deploy merged main, migrate DB, restore portal — 2026-11-26

User authorizes starting Jenkins verification, fixing failures, deploying the
matched web/UI, database and runner/worker release, and restoring the portal.
Start from merged main e9b29210 including timeout fix 8a86dd4. Preserve existing
workspaces, task/Plan/review history, credentials and independently configured
agent roles. No database reset or legacy digest-column drop is authorized.

Deploy matching web host/UI before or together with the runner. Install the
matching Jenkins verifier before any coordinated switch to transmitting named
contracts; leave that producer switch separate pending the requested handoff.
Keep genuine failed CI results and qualify each changed exact source. Read-only
preflight found schema40, API runner inactive/disabled, other worker and web
services active. Jenkins #96 failed source preflight on a contradictory
prepare-source diagnostic assertion. ABI16 release-actor admission and two stale
M39/M41 database grant guards are being corrected before final qualification.
Use isolated source worktrees and /mnt/data evidence; inspect leases before
service cutover and preserve a verified DB backup and rollback selectors.

# Previous: Reconcile the system picture and delivery direction — 2026-11-26

User requests reviewing the consolidated changes and updating files/notes to
reflect the current system and where it is headed. Update the existing status,
roadmap, architecture and operating references on backup/main / draft PR29.
Use application code 0163a615 and dated read-only runtime evidence. Separate
framework primitives, the concrete Python/PostgreSQL delivery application,
installed configuration, qualified artifacts and future proposals.

Observed schema40; event/API runner inactive/disabled; coding/publication/
external-delivery/web/source-agent/Jenkins services active/enabled. Source adds
ABI16/schema41; historical fingerprint and actual-browser qualification remain
open. Preserve dated historical evidence and existing migration files. This is
a documentation pass, not a deployment, service-start, test-qualification or
workflow-policy change. Review links, YAML and contract references, then publish
the notes through the existing PR.

Documentation review reconciles the newly published resubmission queue from
main 4bf1d230 without changing its original task YAMLs. Runtime read-only
observations, 25 added/changed local link checks and YAML parsing are recorded in this
pass; application code and previously recorded test results are not requalified.

# Previous: Consolidate local development and remaining feat/fix branches — 2026-11-26

User authorizes merging every local development branch plus remote
`feat/forge-ide-agents-triage` and `fix/native-postfix-verification-20261125`
into `backup/main`, then deleting those 30 local and two remote branch names.
Preserve `main`, remote `feature/muse`, all worktree directories and their
uncommitted/untracked contents. Named development worktrees become detached
at the exact preserved branch tips after remote backup verification.

Most local changes are patch-equivalent to existing cherry-picks; integrate their
original histories without reverting newer changes. Include the remaining private
CI recovery helpers and merge Agents triage with the CI/CD stage inspector.
Record branch tips and validation in
`docs/reports/local-branch-consolidation-20261126.json`. This source consolidation
does not authorize deployment or restart the boot-disabled event runner.
Validation: 326 Rust native tests, 35 generation tests, 18 CI recovery tests,
and 7 relay tests passed. The generated input-presentation fixture passed.
Historical source-fingerprint qualification remains outstanding. Browser
acceptance stops at the missing locked Playwright dependency; do not claim
browser or screenshot acceptance.

# Previous: Consolidate task branches into backup/main — 2026-11-26

User authorizes merging all remote `forge/task-*` branches into the existing
`backup/main` branch, then deleting those task branches. The source branches and
exact tips are retained in `docs/reports/task-branch-consolidation-20261126.json`.
All 32 task tips must remain ancestors of the published backup before deletion.
Main, unrelated branches and task worktrees remain preserved.

The merge combines persistent agent branches and CI stage reporting with the
newer content identity and author handoff code, retaining superseding integrated
plan-override and terminal-settlement implementations. The task migration uses
0041; applied migration files 0001 through 0040 remain unchanged. Targeted backend
integration: 129 tests passed; 9 author-handoff schema tests and 316 Rust UI
tests also passed. Historical source-fingerprint checks fail against
the combined source; this archival integration is not a release qualification.
No deployment is performed. The event runner remains stopped and boot-disabled.

# Previous: Durable AGY-to-Codex quota handoff — 2026-11-26

The user authorizes adding an automatic handoff to Codex when AGY quota blocks
work. Root owns integration, typed handoff/transition ports, qualification and
delivery. Separate writers own catalog configuration, additive SQL delegation,
and immutable peer-feedback compatibility. Each uses an isolated worktree.

Start from verified main f43a4cc6. Keep original workflow snapshots, approved
Plans, stopped-source receipts, task/workspace identity, feedback and per-event
retry counters. A completed, source-bound delegation action selects the future
Codex author while the original reviewer remains independent. The Codex author
profile is distinct from the reviewer profile; reviewer sessions remain fresh.
Never reuse an AGY conversation in Codex, grant approval through a handoff,
manufacture CI, reset budgets, invoke paid-credit fallbacks or poll account APIs.

Cover both published quota-stopped authors and unleased queued work with a
genuine unresolved quota source for its selected AGY model. Serialize handoff
against claim/cancellation, require fresh selected Codex/reviewer readiness, and
commit delegation, task recipe and checkpoint atomically. Retain normal peer
review, exact-source Jenkins and automatic merge. New configuration is optional;
unconfigured workflows retain their existing behavior.

Live activation requires matched schema, scoped grants, sealed runtime/config
and clean worker ownership; source publication alone is not activation proof.
Record actual test, publication, recovery and running-source evidence separately.

Candidate checkpoint: thirty actual handoff tests and six historical controls
passed, including M32 upgrades, original 49-character review escalation, Opus
continuation, queued Plan revisions, partial repairs, twenty-per-event deadline
and Codex quota boundaries. Three delegated-author recovery regressions failed
before the correction and passed afterward. M40's nine schema tests include a
deterministic claim-cursor race that fails on M39 and passes on M40.

The first full local gate stopped on historical source-hash assertions, not a
handoff behavior failure. The explicit M40 qualification overlay preserves all
34 historical maps and all 39 predecessor migration files. Forty-seven combined
boundary/parity/CI-contract checks passed against the finalized source. The
normal observed-main Jenkins run must now establish the complete final-source
backend/browser/native gate before runtime activation. Private operation drafts
remain inactive until that protected receipt exists.

# Previous: Forge IDE Agents triage (UI-01 / UI-02) — 2026-11-25

Implement a focused review PR on `feat/forge-ide-agents-triage`, starting from
fresh `origin/main` 32588ab5. Surface the existing authoritative Needs attention
scope, readable assignment rows, and a single Now / Why / Next task summary.
Keep retry budget separate from revision count and worker readiness separate
from main verification. Preserve native/Muse semantics, review and merge gates,
full evidence, selection, keyboard focus and list position.

Root owns typed component/YAML declarations, reusable presentation primitives,
integration, qualification and the PR. Model and owned-fixture work use separate
worktrees with bounded ownership; readers may audit without edits. Apply Design
Forge UI, Design Forge IDE UX, Design Modern UI and Test Forge UI. No handwritten
application HTML/CSS/JS, workflow-policy changes, unrelated screen work, live
assignment mutations, deployment, main push or merge are authorized.

Extend Rust tests and the fixed `make ui-qa` fixture journey for native/Muse,
automatic retry versus operator action, keyboard/focus/scroll and desktop plus
320/390px layouts. Capture and inspect fixture before/after inbox/detail images.
Local Rust/browser prerequisite availability and hosted CI are being checked;
report actual checks, source identity, blocked steps and unverified live behavior.
Only the named feature branch is to be committed/pushed and opened for review.

Implementation checkpoint: the Rust triage projection and authoritative scope
shortcut, retained list selection/context, typed list/detail navigation primitive,
YAML layouts and owned-fixture coverage are integrated. No backend policy or
action guard changed. Local `make ui-qa-check` passes 37 tests; the script suite
passes 129. The exact MAIN emitter passes the existing and extended deterministic
input-presentation suite (a Node diagnostic, not generated/WASM/browser evidence).
`make ui-qa` stopped at prerequisites: Cargo is absent, as are the locked browser
dependencies. `make portal-style-check` likewise stopped at Cargo/rustfmt.
No dependencies were installed and no real assignments were used. Fixture PNG
capture and actual browser acceptance remain unverified. The feature branch is
published as review PR #23. A read-only GitHub query confirmed both Rust CI and
IDE browser acceptance are disabled_manually; no hosted run or status exists for
the published source. That policy remains unchanged. Even when enabled, ordinary
PR browser jobs skip the full journey. The connected tools provide no
authenticated workflow-dispatch path.

Formatting follow-up: the official Rust Playground formatter processed 16 changed
Rust files with edition 2021, correcting 7. Child module declarations were
temporarily opaque for single-file formatting and restored unchanged; no skip
attribute was added to the repository. Independent review found no semantic or
literal changes. Automatic approval review then blocked further external source
uploads because that separate service was not explicitly authorized. That route
was stopped; the remaining 2 files passed local formatting review. No toolchain
was installed. This is not a Cargo formatting, native test, generation, WASM or
browser gate. Those checks and source-bound before/after screenshots still need
a prepared runner. Live portal verification remains for Dot after Leo approves
the next step; no deployment or main change occurred.

# Previous: Stabilize the event stream and measure bottlenecks — 2026-11-25

User requests fixing the live event stream and identifying its bottlenecks and
errors. Root owns integration, scoped operational recovery, final qualification
and publication. Independent readers trace native workflow/review/retry state,
external Muse assignment/delivery state, and Jenkins/post-merge/release timings.
Writers receive separate worktrees after a cause and reproducer are established.

Start from origin/main 0ed84188. Preserve original owners, approved Plans,
source artifacts, immutable receipts, real cancellation and twenty attempts per
event. Do not reset exhausted counters, manufacture successful checks, replay an
uncertain mutation or switch model providers without existing authorization.
Use typed selected recovery/provider ports, bound transient retries and establish
actual service/readiness state before resuming work. Current-state scans remain
the durable delivery mechanism; NOTIFY is only a wake-up hint.

Initial evidence at20:44UTC: Jenkins queue is empty, no builds are running, and
latest exact-main build86 passed. Nine native workflows escalated after author
invocations failed in a burst at20:34; most invocations lasted about3.5seconds.
The shared CLI failure is confirmed: Antigravity's selected Gemini model returned
RESOURCE_EXHAUSTED / Individual quota reached, which the old parser mislabeled
failed. Most retries then failed within four seconds. The user explicitly selects
Claude Opus5.5 through agy as the backup, while retaining the Gemini primary.
Versioned optional backup configuration and source-bound policy overlays must
preserve original workflow snapshots, Plans, source receipts and model history.
Actual quota failures pause exhausted model admission; configured backup routing
and reset waiting are bounded separately from compilation/test repair events.
No AI-credit fallback or automatic consumer OAuth polling is authorized.

An explicit interactive /usage read at21:00UTC observed Gemini weekly9.57% /
five-hour100%, and shared Claude/GPT weekly98.30% / five-hour97.60%. /credits
reported credit spending disabled. These are manual TUI observations, not a
qualified headless allowance API. The official installed CLI is now1.5.17;
stage an immutable copy for workers rather than a self-updating executable path.

Muse's
only unfinished external workflow is b3a05d8a in Repair with a fresh owned lease,
but no new submitted artifact or progress checkpoint from its current assignment.
Repeated native-Jenkins reconciliation warnings are two retired, inactive actions
with genuine never-submitted cancellation receipts. Add narrow historical proof
recognition plus bounded correlated error diagnostics; preserve the mutation
transport pins and every old binding. CI build86 spent27m14.6s of38m45.7s in the
sequential main backend suite; cache HITs and4.49s deployment rule out cold builds
and activation as the main delay. Bounded whole-module test processes must retain
every unique test and exact execution inventory; no skips or gate substitution.

Muse's last repair took440.3minutes without a progress checkpoint, then main
drift caused another author turn. Trusted private Git preparation should integrate
clean main changes, freeze the selected tree/base, and obtain fresh independent
review and exact-source CI. Only genuine conflicts need the original author.
This does not claim control over the externally owned Muse execution speed.

Qualification checkpoint: the combined suite caught a fresh-worker startup
ordering bug in cooldown storage. Follow-up f999518 acquires only the owned
selected state directory at first advertisement, with anchored no-follow
ancestor validation and no factory effects. Forty-three combined boundary,
authority, cooldown and normal-runtime integration checks passed with zero skips.
Full backend qualification uses the actual CI Cargo1.97.1 toolchain rather than
the older default1.86.0. Source is checkpointed before that expensive check.
Matched V3 controller/helper configuration and immutable AGY1.5.17 are staged;
services, source-bound recoveries and publication remain pending qualification.

Additional live causes confirmed: Muse's exact submitted candidate contains two
tracked generated bytecode files. Permanent artifact guard rejection was being
reported as temporarily unavailable forever. The selected delivery provider now
returns bounded path/code diagnostics through the existing failed-preparation
checkpoint and twenty-per-verification-event same-author repair flow. Real Git
and PostgreSQL tests cover both files, unchanged approved Plan, one repair
admission and the twenty-first denial; the guard remains strict.

Two further fresh Plan tasks stopped before model execution because AGY's
user-local program changed after worker startup. Both idle native workers were
quiesced while qualification finished, preventing more pin-refusal failures.
The separate private stopped-binary recovery requires the unchanged exact Plan
recipe, genuine failed/stopped/reconciled source, immutable verified binary,
root-sealed installation evidence, fresh selected worker incarnation and reviewer
readiness. It adds neither public override nor model approval. All original
attempts, scope, source identities and retry counters remain preserved.

Operational evidence lives under
/mnt/data/homelab-build/event-stream-stability-20261125-evidence. The final full
main-backend run records frozen source before/after, actual unique test inventory,
zero-skip suite summaries and exit code; matched cutover and private recovery
have separate receipts. A sealed deployment receipt, not this source checkpoint,
establishes actual activation. Source-observation owns subsequent genuine Jenkins
main verification and portal artifact release; no successful gate is synthesized.

Live checkpoint at21:57UTC: qualified source2ee824e is sealed and selected for
all five API/coding/publication/external-delivery services, with the matched
3a2b2c238f3186d0 privileged verifier. API health200, stable service identities and
fresh selected worker readiness passed. Main publication is remotely verified;
canonical main was fast-forwarded without removing its untracked caches.
The exact four-suite local backend gate passed1568tests with zero skips/failures;
its1557 agent-runner IDs exactly matched discovery and execution. Elapsed10m21s
is a local observation, not a new Jenkins timing claim.

Typed recovery resumed nine genuinely quota-stopped sources on the configured
Opus backup and two separately attested binary-pin failures on their unchanged
Gemini recipe. Original Plans, model history and existing counters remain intact.
The first real Opus Plan completed in6m40s, Codex reviewed it in about32seconds,
and a requested revision was queued automatically at the retained3/20 planning
event. Its live attempt settings are readonly0400 withuseG1Credits=false.
Muse's permanent artifact rejection now produced verification repair1/20; Muse
claimed that same-owner assignment. A lease proves ownership, not completion.

The SourceObserver automatically queued exact-main Jenkins87, without a manual
submission. Its first45checks passed, including browser journeys; the final
backend check exposed a process-group cleanup-observation race in the new runner.
SIGKILL delivery alone did not prove a surviving descendant had stopped before
the already-exited leader was reaped. Release correctly remained blocked. The
follow-up must confirm bounded group quiescence while retaining leader ownership,
keep the existing assertion strict, and pass the actual Jenkins user/environment.
Live application workers remain healthy; this is a CI runner correction. Full
new-main verification and portal activation still require genuine fresh receipts.
Sanitized live state is recorded in the evidence directory'sdelivery-summary.json;
root-sealed activation/recovery receipts are under
/opt/homelab-forge/operations/event-stream-stability-2ee824e9a722.

The process-group follow-up now observes bounded /proc group state before
reaping the retained leader. Unknown, malformed, oversized or timed-out
observations fail closed. A deterministic delayed-KILL regression reproduces the
old failure; the existing immediate no-live-descendant assertion is unchanged.
Thirty focused checks passed as Jenkins userforge_ci UID990 inside the actual
agent mount/PrivateTmp and isolated network environment. Independent lifecycle
and permission/deadline audits passed. This follow-up changes only the CI runner,
its tests and qualification/report records; installed application modules remain
byte-identical to the live2ee824e release. A fresh source-observed main CI run
must establish the complete new-source gate; no failed87 receipt is rewritten.

The exact32588ab local full backend rerun passed1571tests, zero skips/failures;
1560 agent-runner IDs match execution. Jenkins88 passed all45 earlier checks and
its17 process lifecycle tests, then its executor service was explicitly restarted
at22:31:22, killing the final backend check. This was a remoting disconnect,
not a reported failed test or OOM. The old protected progress contains exactly
45completed checks and one started check with no exit; do not invent its exit.
A separate SSHAPT rclone installation overlapped the restart. The installed
Ubuntu needrestart hook makes this a strong correlation; no recordedDBus caller
proves attribution. The targeted exact-unit needrestart override now defers
package-triggeredCIagent restarts; package updates/other services remain unchanged.
It was installed through the reviewed policy-only mode without restarting a unit.
The interrupted action requires source-bound stopped-process cleanup and a truthful
FAILED/infrastructure receipt before its ordinary cancel consumer can finish.
A fresh legitimate source observation must obtain complete CI; no interrupted
receipt can authorize release.

The running Opus backup also genuinely exhausted its provider allowance at22:08.
Its workflow remains enabled inImplement, its approved Plan and stopped workspace
are bound, and its process lease is released. The coordinator recorded one quota
wait with0reset resumptions used; provider reset is2026-11-26at01:53:09UTC.
The durable model cooldown withholds Opus while Gemini work continues. No paid
credit fallback or repeated model probes are used. This is an allowance wait,
not human attention or a lost event. Account percentages remainunsupported_headless.

The scoped package-restart deferral was installed at
/etc/needrestart/conf.d/99-forge-ci-agent.conf (root0644), without restarting
any service. Its exact service pattern and dedicated installer mode passed
focused Perl/installer tests and independent audit. The reviewed private CI88
interruption cleanup passed readonly preflight, then ran under exclusive locks.
It archived original evidence, proved the retired executor invocation and idle
build account, performed real cleanup, and wroteFAILED/verification_unavailable
with45observed completed checks and explicit unknown final exit. The ordinary
cancel consumer then closedde0e59bd asfailed withcleanup_confirmed=true and
receiptSHA4e26170138bb76240f7e81e2fd11ae3a57ae52fe77ed09c1173eca12806cb80f.
No task or CI success was synthesized; the fresh main must still pass its gates.

# Previous: Git-independent artifact and verification identities — 2026-11-25

User requests implementing the content-identity design described after task
recovery. Separate stable task UUIDs, canonical source-content SHA-256 manifests,
and explicit frozen verification-policy/generation identities. Git object IDs,
branches, diff presentation and checkout roots are provenance, never artifact
identity. Persist a job's selected identity and policy snapshot before effects;
resume against that snapshot. A deliberate policy change requires a fresh
verification generation rather than rewriting an old receipt or CI index.

Root owns integration, external-review wiring, source qualification, acceptance
and final publication. The manifest component owns pure contracts, selected
source adapters/factory and meaningful cross-source tests. The verification
component owns normal native review/publication wiring and backward-compatible
snapshot persistence. Each writer uses an isolated worktree. Document the actual
types, lifecycle and failure behavior with UML component/data/sequence diagrams.

Preserve existing v1 jobs, approvals, source bindings, migrations, mutation
transport pins and per-event retry budgets. A content match does not authorize
reusing an approval for another workflow or skipping exact-candidate CI. Do not
rewrite historical identities, reset live tasks, restart active workers, or
claim that source publication is a portal rollout. No paid agent calls are
needed for this implementation.

The source implementation now contains pure typed manifests, bounded selected
Git/directory/memory source adapters, shared crash-safe snapshot storage, native
review/CI/publication wiring and external preparation/reviewer wiring. Native
selection requires content-v2 plus an explicit UTC admission cutoff; external
selection is frozen at workflow creation. Existing delegation, SQL/migrations,
exact-candidate Jenkins transport, approval rules and retry counters are retained.
Four UML views and a Rust-generated Design reader document the implemented path.
Full qualification and actual source/deployment identities are recorded separately
from component tests; this checkpoint activates no live protocol or worker.

Source e49b02ae passed the complete 1,477-test runner inventory without omissions
or skips, all 67 host-consumer tests, and the three supporting backend suites.
Its content-v2 protocol remains opt-in. The prior live CI failures were deployment
drift: the installed verifier omitted two M38 protected paths already present in
the runtime and repository. A new immutable matched helper (2dd31c5e) and paired
selectors are now active; old helpers, failed receipts and spent counters remain.
This deployment record also admits a private once-only verification recovery for
the exact exhausted native source after that proven correction. It keeps twenty
automatic admissions and appends an honest manual verification21, without a
code/Plan approval or an author turn. Fresh protected CI must establish success.

# Previous: Recover cancelled/parked Muse and Gemini work — 2026-11-25

User asks to fix the cancelled Muse task and the attention-state Gemini portal
CI/CD task, determine why they stopped, and let their original owners continue.
Muse root 6fdff749 / external workflow b3a05d8a exhausted twenty code reviewers
before any model started: identical patches had different Git index-line
abbreviations across producer/reviewer clones, so the diff digest mismatched.
External escalation stopped linked tasks as cancelled; the list badge then hid
the authoritative escalated workflow behind that raw cleanup status.
Gemini root 49227db2 / native workflow c87bf342 stopped at its thirty-minute
execution deadline twice. Its workspace and approved Plan remain preserved,
but the native-timeout scanner allows only one automatic retry per workflow.

Root owns integration, source qualification, tested immutable runtime rollout,
private audited operator recovery and live event verification. External owner
fixes canonical full-index diff production/validation and a scoped code-context
post-fix recovery that retains the exhausted twenty-attempt evidence. Native
owner uses the injected retry-event provider for bounded deadline episodes,
including validated existing retry history, without a migration or budget reset.
UI owner fixes the Rust authoritative-workflow badge and verifies generated
assets; genuine user cancellation remains cancellation. Preserve each source
artifact, reconciliation, Plan, owner, receipt, and protected CI requirement.
No direct SQL status resets, stale-context approvals or fake completion.

The portal release also exposed a pre-existing Jenkins observation ceiling:
the old verifier cancellation kept retrying because a fifty-build snapshot was
truncated. Add typed bounded build pages and an explicitly selected, pinned
read-only observer. Preserve every frozen mutation executable/configuration and
its journal binding; the new observer may read complete stable history but may
never POST or infer absence from incomplete/changed pages. A real read-only
probe has covered both live pages and established the old action is absent.
Normal coordinator cancellation and main verification must supply actual proof.

The task-recovery source is published on main at bf080e2c; the paged Jenkins
observer is published at 41bb8ea5. The complete 1,423-test runner inventory
passed on bf080e2c with no omissions or skips. The Jenkins addition passed
146 Rust tests and 120 focused Python integration tests, formatting and Clippy.
Current-main CI remains a separate required verification, not inferred from
those earlier results. Backend recovery and the read-only observer are active;
frozen Jenkins mutation executables/configurations remain unchanged.

Muse's reviewed post-fix rescue retained the twenty failed review admissions
and queued revision37c70ee3 under the same owner and approved Plan. Muse has
claimed that revision; workflow b3a05d8a is Repair/enabled. The explicit handoff
is to run this retry, without approve-override. Gemini completed its recovered
attempt and its first automatic merge-conflict repair. Switching the publisher
runtime interrupted verification9a671d6c: the updated gate contract produced a
different action identity for an already indexed review. Preserve that index,
the pushed candidate and the confirmed unstarted/cancelled old action. A narrow
typed, source-bound recovery must append a new review and consume the existing
infrastructure allowance; it must not overwrite the index or restart the author.

The user's Git-agnostic identity direction separates stable task UUIDs, canonical
content SHA-256 manifests, and versioned verification policy/generations. Git
branches/commits remain checkout and merge provenance. Hash exact sorted paths,
modes and file bytes for artifact identity; presentation patches and runtime
enumeration must not silently redefine an existing identity. The current
full-index patch correction preserves the existing external-review protocol;
a complete content-manifest protocol transition is distinct work, not claimed
by this verification-only recovery.

Verification-context recovery is a private CLI with a factory-injected workflow
store and read-only stopped-action proof port. It appends through the existing
bounded preparation authority and clears only the successor's candidate/checks.
Confirmed registration resolves only its own infrastructure event; the receipt
explicitly retains verification_pending=true and approval_granted=false. No
service restart or gate-contract change is required for this operator recovery.

# Previous: Reconcile feature/muse review-schema compatibility — 2026-11-24

Fix the remaining py-agent-runner failures on feature/muse at e41db88, based on
main dc1af267. Preserve main's tested superseding-review provider byte for byte:
its parent remains escalated, recovery discovers escalated rows, and the
49-character context-hash regression continues to expect escalated.

Retain M33's source-binding constraint including review_generation and its
canonical lineage guard. Reconcile the branch-only M35 correction path with
that contract, keeping its final superseded status confined to that path.
When the canonical correction provider and retry accounting are selected, the
compatibility scanner yields to that provider instead of spending a second
correction budget or changing the canonical parent's status.
M35 is unmerged branch source; migrations 1–34 remain unchanged. Regenerate only
the affected M35 catalog expectations and explicit changed-source pins.

Persist optional peer diagnostics only when the trusted locked review row
exposes that column. A schema32 workflow still records the ordinary escalation;
schema35+ retains structured diagnostics. Use actual PostgreSQL regressions for
both cases and the complete agent-runner suite without skipped cases.

Root owns the feature/muse worktree, integration, task record, source pins,
full-suite qualification and final branch commit. SQL and diagnostics writers
use separate worktrees; an independent reader audits the merged contracts.
No production migration, deployment, main merge or model invocation is part of
this branch-only fix.

# Previous: Fix reviewer protocol and CLI transport causes — 2026-11-24

The user requests finding and fixing the two actual review failures. Native
diagram workflow3939 passed candidate CI, then its critic returned `revise`
with approval-only attestations. Preserve that verdict and all findings; improve
the prompt and request a new independently valid review through the existing
bounded superseding-review provider. Do not normalize or accept invalid bytes.

The external Postgres-proxy workflow8c300 exhausted its reviewer-correction
event at 20/20 after CLI JSON transport parsing failures. A bounded read-only
supervised diagnostic reproduced the failure after 54.8 seconds: Codex 0.160
serializes both the outer item ID and web-search ID under the same JSON key.
The 153-byte rejected record has SHA-256
`547a2be149ecb96f6dfecf9e172cd3138b8aa9113f9f87b3efdd9537e31a4076`.
The narrow transport adapter accepts that exact root web-search metadata alias,
preserves the outer ID, and rejects duplicate control, result and nested fields.
Its actual-record replay and chunk-boundary regressions pass. Private structured
failure logging records only a fixed reason, redacted path, size/hash and attempt
identity. The diagnostic applied no verdict or DB mutation; strict verdict/source
parsing and frozen execution flags remain unchanged.

After the qualified fix is active, the user's new instruction authorizes one
explicit private operator recovery review of the exhausted external event.
Preserve20/20, all original source artifacts and owners, and record the exception
once through the existing receipt-bound recovery port. It is not another twenty
automatic attempts. A genuine valid review may resolve the same exhausted event;
a failed exception leaves it exhausted. No applied SQL edits or new public route.

Root owns integration, exact-source checks, safe idle-worker rollout, publication
and live proof. Native-review owner fixes the classifier/prompt; transport owner
proves and fixes the CLI boundary; recovery owner implements the narrow private
post-fix exception. Each writes a separate worktree. Keep Muse's other active
work and all running review processes uninterrupted.

# Previous: Continue verified peer feedback automatically — 2026-11-24

The live superseding-review recovery ran Codex through its normal worker and
obtained a valid `revise` verdict for the original stage-diagram candidate. Its
reviewer-protocol event resolved at one of twenty attempts. The subsequent
author setup exposed a distinct defect: a linked child receiving a review of
itself was incorrectly bound to its parent's older source review. Correct only
that source relationship; preserve strict parent binding for feedback that
actually targets the parent and retain every other identity and digest check.

Select a separate peer-feedback recovery provider at the workflow composition
root. Before normal Resume it must verify the actual stopped attempt,
reconciliation, unchanged source and feedback, independently valid decided
peer revision, approved scope, current policy, readiness and idle admission.
Reuse the server-issued continuation note so the preserved feedback remains
bound across a setup-only retry. Keep the build-feedback recovery semantics
unchanged. Repeated recovery consumes the same feedback-delivery event's twenty
attempt allowance; duplicate or raced admissions consume once. Never rewrite
the valid verdict, failed attempt, original owner or spent counters. No schema
change is required; applied migrations1–33 remain immutable.

Root owns integration, source qualification, safe idle-worker rollout and
publication. Core owns the narrow pure validator and full provider regression;
Audit owns the scoped automatic recovery port and actual PostgreSQL regressions.
Ops stages release and CI configuration only; Root activates qualified artifacts.

## Component: one post-fix external review after transport exhaustion

The user requests finding and fixing the current review failures. The proxy
Plan's reviewer-correction event is exhausted at20/20 after CLI transport parsing
failures. Extend only the existing private deployed-fix recovery port: authorize
one explicitly recorded operator review exception after the qualified fix, with
all twenty automatic admissions, original Plan/roles and stopped attempt history
retained. Read and lock the exact terminal event through the already selected
private grant provider; grant no new SQL/model authority. A valid fresh review
resolves that same event; failure remains exhausted with no automatic twenty-first
retry. Preserve the historical one-retry immutable-context recovery unchanged.

The recovery component owns its private provider predicate, actual-role
PostgreSQL regressions and external workflow documentation. Root owns the real
transport correction, integrated source qualification, deployment and the actual
explicit recovery command. The component performs no live state mutation or
model invocation. A guarded preflight against actual production data reached the
first write boundary with every guard satisfied and identical before/after state.

# Previous: Recover malformed native reviews automatically — 2026-11-24

The user requests actual automated completion of the original stage-diagram task,
not another status report. Preserve original Gemini/Antigravity author and Codex
reviewer identities. The native workflow3939c2a5 is disabled after reviewer719aa2b2
returned a49-character context hash against a64-character immutable target.
Never repair or accept those result bytes. Add a bounded, factory-selected
superseding independent review, preserving original artifacts, plan, candidate,
checks, snapshots and event budgets. Existing immutable review identity uniqueness
requires additiveM33; migrations1–32 and applied fingerprints stay immutable.

Core owns backend port, schema33, exact source/grant verification and focused
regressions in this worktree. Audit owns independent PostgreSQL tests in its own
worktree. Root owns source-qualification/CI protection integration, publication,
backup/migration/runtime rollout and normal recovery of the exact original task.
Use20 attempts per logical reviewer-protocol event; superseding review UUIDs must
not reset the same event. Duplicate and raced recovery admissions consume once.
Cancelled/revoked/busy/drifted scopes remain fenced. Real independently valid
review and existing CI/merge gates remain required. Muse tasks retain their original
owners and dispatch requests; do not claim or restart Muse on its behalf.

The source implementation now selects a narrow superseding-review provider at
the normal WorkflowStore factory. It classifies only a context-digest fault;
all other strict verdict fields and attestations must already validate. Original
verdict bytes are never corrected or accepted. AdditiveM33 enforces immutable
generations, one successor and one live review per exact source. The replacement
uses ordinary review actions and a different native reviewer task. Existing
scoped grant capabilities are reused without widening application/model ACLs.
The shared reviewer-correction event uses the root review UUID through the
entire chain. Source/Plan/check evidence and actor identities remain unchanged.

Component verification:24 pure classifier/peer/factory tests and53 owned
PostgreSQL schema/upgrade/security tests passed with zero skips. Independent
actual-role recovery and normal legacy lifecycle qualification are recorded
separately; these source checks do not claim live activation or task completion.


# Current follow-up: Preserve repair lineage when the observer wins — 2026-11-23

M31 is live and strict verification passed with unchanged Muse history. M1–31
are immutable. The independent PostgreSQL ordering probe found a remaining
normal-repair race: an observer-created verification could keep a fresh lineage
when its genuine repair merge was confirmed later. Add M32 rather than rewriting
the applied migration. Adopt only a proven, unused provisional lineage; preserve
verification identities, receipts and all spent attempts. Conflicting used
scopes need durable evidence and an admission fence, never counter resets.

Core owns M32/schema compatibility; Audit owns independent PostgreSQL regressions
and future CI protection. Root owns integration and the exact-source rollout.
The prior main4da verification retains its frozen installed verifier; no
candidate may change a running build. Future helper/policy changes require their own immutable pins and
qualification. Continue Muse through its recorded planning event only after the
matching runtime and ABI15 portal are verified, retaining its three spent tries.

Component verification passed64 upgrade/security/schema cases,24 delivery
regressions and4 independent observer-first cases. The latter retain the same
event UUID through20 normal admissions and refuse21, preserve a prior exact PASS
and fence conflicting used scopes. Full integrated qualification and the actual
runtime/deployment receipts remain separately recorded operational evidence.

# Twenty tries for each failure event — 2026-11-23

The user corrected the earlier interpretation: "20 tries per event", for every
agent. Compilation fixes, tests, and other independently failed phases have
separate budgets. A task may therefore exceed20 total repairs. Aggregate plan,
code and delivery counts remain history and must never be reset or used as a
task-wide20 ceiling.

Use a durable event UUID and an independently verified phase identity. New
builds, commits, findings text, deliveries and agent handoffs retain the same
unresolved event. Reserve a retry atomically with work admission; duplicate
delivery consumes once. Only a protected passing phase receipt closes the event.
The21st retry for one unresolved event requires attention; another event retains
its own20. Quota, permission, cancellation and stopped-process guards remain.

Core owns the common factory/DAO, additiveM31 and native integration. External
owns database-agent orchestration, continuation and ABI15 UI/transport. CI owner
owns protected v3 phase execution; Root owns delivery lineage integration,
privileged proof validation, source qualification and release. Use separate
worktrees and preserve all saved approvals, reviews, hashes, counters and
receipts. M1–30 are immutable. M30 was applied with no Muse history changes;
the superseded task-wide runtime/configuration was not activated.

Component checks prove20 compilation repairs followed by20 test repairs in one
native workflow, external workflow and delivery lineage. The21st retry of the
same unresolved event stops. All154 core native/upgrade/security tests,38
external transport/continuation tests,80 CI contracts and30 reviewer/feedback
event tests pass. The explicit v3 Jenkins installer retains artifact export;
all15 installer contracts pass. Fresh live-schema30 backup was actually restored
and upgraded to31 in isolation with strict verification and identical Muse
history. Full integrated Rust/browser/backend gates and guarded deployment are
separate recorded operational work; component checks do not claim deployment.

Integrated regressions also retain native workflow IDs in raw/scoped list
projections, preserve lifetime rounds when an event exhausts, and authorize
feedback for an existing repair child only through its exact saved peer-review
event admission. Historical worker feedback branches and grants stay unchanged.
Freeze source before importing test modules; retain failed integration logs as
evidence rather than counting a pre-freeze module snapshot as final validation.

# Previous, superseded interpretation: task-wide twenty-attempt policy — 2026-11-23

The user explicitly clarifies that the twenty-attempt allowance is agent-wide,
including Muse, native authors and independent reviewers. Use one provider-neutral
retry policy for plan, code and reviewer correction; remove provider-specific
defaults that stop these loops at three. Preserve frozen delegation/recipe hashes,
all prior reviews and spent counters. Apply additive schema30 for the existing
database bounds; never edit migrations1–29. Continue the stopped Muse plan under
a durable policy-update event, retaining its three spent revisions and original
Codex feedback. Root owns UI protocol bounds, source qualification and live rollout;
shared-policy owner owns native/SQL; external owner owns Muse continuation; audit
owner owns reviewer-correction and manual-history compatibility. Process timeout
durations and OS rollback limits remain separate from agent revision allowances.

Integrated source checkpoint: shared policy, native amendment sweep, M30,
external continuation and reviewer/manual-history compatibility are implemented.
Legacy source-readiness exhaustion uses the frozen or amended threshold while
retaining its once-only delivery guard. Portal protocol accepts historical and
20-attempt limits without a new ABI. Component UI verification passed113 native
tests and28 browser groups; integrated source preflight passed221 tests with zero
skips. Actual PostgreSQL tests cover20→21, immutable snapshots/counters and raced
continuation. Final exact-main Jenkins proof and protected deployment are separate
live gates; no task history or allowance was changed by this source checkpoint.

Follow-up qualification: all206 native UI tests pass, including the complete
integration targets and zero-budget/20/21 boundary. The additional44 Python
reviewer, worker and delivery contracts pass without skips. Two pre-release test
corrections (Rust formatting and the historical upper-bound assertion) retain
their failed Jenkins receipts; redundant repair tasks were cancelled through
normal WorkService admission and confirmed stopped. Production policy and
recipe identities remain as reviewed. M30 live strict verification and backup
restore evidence are recorded outside Git; matching UI/runtime activation remains
a separate exact-main gate.

# Previous: Recover feedback delivery through the event stream — 2026-11-23

The user explicitly requests implementing recovery for the stopped diagram
workflow. Gemini's stale build-results directory was corrected in a versioned
live config; the existing task remains at revision12 with a stopped, reconciled
attempt and immutable native feedback. Add a factory-selected, bounded recovery
consumer and durable recovery event, preserve the approved plan and all prior
attempts, and resume through normal WorkStore admission. Never fabricate a
timeout, rewrite feedback or reset review budgets. Worker I/O remains outside
the DB-only coordinator; it must still validate actual build evidence before
launching the CLI. Root owns worker/context integration, source qualification
and activation. Backend owns the isolated recovery port, contract and regressions.

# Previous: Complete the event-driven SDLC and activate live — 2026-11-23

Startup-observation checkpoint (source correction, not a rollout): the existing
`Type=simple` service can report Started before exec. The portal actor now observes
only its exact requested command/configuration and a known systemd pre-exec image
within a persisted ten-second deadline, then requires the unchanged origin/entry
health proofs. Unrelated commands/processes fail immediately; reconciliation does
not repeat activation or reset the deadline. Fixed private diagnostic stage codes
preserve failure context without leaking raw errors or secrets. Eight new owned
startup regressions plus existing actor/recovery/provider tests pass (44 total,
zero skips); no service, database or prior terminal receipt was modified here.

The user explicitly authorizes completing all five listed gaps: live v2/M28+
activation, normal-policy automatic failed-main repair, portal lifecycle visibility,
guarded automatic deployment, and capture of main changes outside coordinator
merges. Complete and prove the loop; do not stop at source-only partial work.
Latest user steering sets the shared failed-main repair lineage cap to **20**
for now. This supersedes the initial three-round cap described in component
checkpoints below.
Use existing PostgreSQL/coordinators and factory-selected injected ports. No NATS,
GitHub Actions, new broker/service, paid API fallback or model credentials passed
to workers. Preserve existing data, completed receipts, worktrees and secrets.

Root owns integration, coordinator composition, source qualification, source
publication, Jenkins qualification and all privileged live rollout/rollback.
Backend owner owns agreed additive schema29/DAO/normal admission/projections;
UI owner owns Rust/YAML/DTO/rendering and browser regressions; ops owner owns
source-only fixed-scope release and main-observation adapters/tests. One writer
per isolated worktree. Agree shared contracts before overlapping changes.

Automatic repair creates NEW work under current WorkService author/reviewer
policy and bounded lineage limits, preserving completed parents. Releases need
exact-main full v2 proof, immutable binary/assets, healthy candidate, confirmed
activation receipt, once-only/reconciled writes and rollback. Source observation
must not fabricate a coding workflow identity. Status distinguishes merge, main
verification, repair and release. Migration28 existing source remains immutable;
additive29 owns extensions. Retain historical migrations and authority guards.
Before expensive gates, inspect changed source qualifications and run cheap
checks. Reuse prior evidence only for verified byte-identical inputs; retain
failed receipts and report coverage honestly. Live activation is now authorized,
including required schema/config/scoped grants and normal-account included model
work for a bounded harmless end-to-end proof. No external emails/messages.

UI handoff: optional bounded delivery DTOs now cover catalog projects and task
summaries/details. Native Work alone requests Forge-Delivery-Version1; new clients
accept legacy payloads without delivery. ABI14 pins host, manifest, worker and
WASM together. Rust projects separate semantic verification/repair/release pills,
relative times, guarded repair links and source disclosures using generated
Rust/YAML components. Completed merge history remains Merged; completed roots
follow current lineage for Active/Attention/History filtering without downgrading
ordinary active authors. The existing watch refreshes pending task/project
delivery. Deployed requires an activation receipt, not merely main CI success.

Focused current checks passed:110 UI library tests,7 host library tests,66 host
consumer tests,31 build-assets generator tests,17 scope/review integration tests,
35 Node harness tests, and all-target Clippy with warnings denied for the three UI
crates (build-assets enabled). Formatting, diff checks and42 scoped documentation
link targets pass. Eight new Rust delivery tests plus native relay and fixture
regressions cover legacy absence, nullable IDs, bounds/source syntax, immutable
merge history, repair links, delivery scope/watch, receipt-based deployment and
observed project main without fake tasks. Full real-browser assertions were
extended for queued/running/repair/deployed at320/390/1440 widths; they remain for
Root's integrated atomic WASM/browser gate. No live activation or full Rust matrix
ran in this component worktree. Current docs retain last qualified live956/ABI13/
schema26 separately from new source support.

Ops handoff: source observer uses the existing trusted source port and persistent
per-project head/CAS, with one project per turn and sixty-second retry budget.
Portal release selection injects only the pinned fixed Root actor and its four
bounded operations; identity/plan/health/rollback proofs are checked before
handoff. Optional v2-main artifact export performs two fixed isolated build
steps after the existing three gates, quiesces the build account, and seals
Root-owned host/assets with a source/request/action-bound manifest. Legacy and
branch gates remain unchanged. Sixty-three focused tests passed with zero skips
in 4.358s; diff check passed. No live operation was executed by this writer.
The integrated Root actor was then exercised with owned temporary files and
injected system/probe ports: activation and replay, crash after restart, healthy
rollback, staged-copy recovery, failed-candidate cleanup and source/proof tamper
rejection. Provider/actor planned-replay handling is included. Final combined
focused run: 71 passed, zero skips, 4.148s. These substitute system calls and do
not constitute live activation evidence.
## Actual HTTP rolling compatibility correction

Live validation found the new Work header rejected by the shared closed header
validator and legacy projection stripping aimed at a non-envelope shape. Fix only
Work routing: validate the extension, preserve all other header multiplicities and
shared validation, and strip known delivery fields inside the actual protocol/event
envelope. Exercise the real authenticated runner HTTP path for old and new
callers, malformed/duplicate/unknown headers and preserved user payload. No schema
change or live operation; Root owns republishing/redeploy and new Jenkins source.
The33-case focused actual HTTP/decode/listener/catalog plus full delivery suite
passed in19.723s withzero skips. This includes real old/new HTTP envelopes and
result content, strict pre-effect negative headers, actual PG legacy/new list
filter-before-pagination, and the20-admission shared-lineage cap.

## Latest user steering: twenty failed-main repair assignments

The user now requests a shared failed-main lineage limit of20. Apply only to
additive migration29 constraints/reservation/admission/projection and the actual
PG regression:20 distinct legitimately admitted repair workflows share one
lineage, the20th is permitted and the21st is refused. Native Plan/code policy
budgets and the Root rollback-attempt limit remain unchanged. Historical
migrations1–28 stay immutable. Regenerate exact M29 schema fingerprints and run
the focused actual-PG delivery suite before commit; Root owns rollout.
The updated18-case focused suite passed in12.532s withzero skips, including
20 distinct normal-policy repair workflows sharing one lineage and refusal of
the21st. Fresh schema29 full strict verification passed with8 tables,39 functions
and the one validated M28 synchronization trigger. No live effects occurred.

## Backend verified follow-up

The complete18-case focused contract suite passed on owned PostgreSQL16 and
pure adapters (11.822s,zero skips), including three real normal admissions with
shared lineage capacity and blocked fourth, lost-submit-ACK recovery without
another task, atomic grant withdrawal, exact native/observer race deduplication,
real-root projection plus pagination and ABI13 list semantics, repeated SHA/new
release occurrence, fresh Root cleanup lease, exact checkpoint/hash, post-revoke
activation facts, entry-bridge/rollback negatives and pending operator attention.
Factory substitution injects the selected ready repair port and disabled config
constructs no unused provider or DB connection. Repair prompts now retain trusted
CI action/build identity first and bound original request/acceptance separately.
No model/GitHub/native build or live operation was used. Root-requested guarded
activation checks and affected historical fixtures continue in separate evidence;
source context qualification and final rollout remain Root-owned.

## Backend initial implementation checkpoint

Owner: backend, isolated `complete-sdlc-20261123/backend`, task-record base5262806.
Drafted additive M29 (8tables,39 narrow/private functions), immutable canonical
verification/source-CAS ledger, normal WorkService bounded repair admission,
atomic task-lineage binding and independent lifecycle projections. Existing M28
functions/tables remain unchanged except an independently pinned M29 sync trigger
excluded only after strict M29 verification. Root-only activation authorization
checks live lease/scope/deadline/source and exposes checkpoint/hash metadata;
its actor_started latch preserves cleanup after pre-checkpoint revocation.
Header1 gates both delivery fields and new list-scope semantics for ABI13 rollout.
No model, GitHub, outbound message, native build or live DB/service action occurred.

Initial actual-PG16 create/reapply/full strict catalog checks and9 focused
integration/compatibility cases pass (5.414s,zero skips). Source migrations1–28
are byte-identical. This is an initial reviewable checkpoint: deeper lineage3,
lost-ack, observer/native-race, scope-pagination and affected historical fixture
checks follow before qualification/activation. Root owns installer publication,
all source context/guard overlays, integration and privileged operational rollout.

## Integrated qualification and runtime evidence boundary

The reviewed source composes one unified verification store and rotating ready
verification/observation/release ports. Current strict source qualifications retain
historical maps and all twenty authority AST guards. Actual ABI14 browser QA
passed all28 groups with cleanup; Root recovery tests cover the fixed entrybridge,
interrupted rollback and operator attention. The protected database backup was
restored into disposable PostgreSQL16:45tables/1,565rows, all counts equal and all
historical task/workflow/receipt contents exact. Current usage/readiness differences
are bounded live refresh/heartbeat fields. The two inherited GitHub Actions
workflows were explicitly disabled for the Jenkins-only operating path.

Release qualification and activation happen AFTER this source snapshot freezes.
Their completion receipts are retained outside Git under
`/var/lib/forge-provisioning/complete-sdlc-20261123/` and
`/var/lib/forge-provisioning/portal-release/`, with working evidence under
`/mnt/data/homelab-build/complete-sdlc-20261123/`. The live Agents delivery panel is
the current runtime projection. Do not publish a documentation-only completion
commit after activation: the main observer would correctly schedule another full
verification/release. This record claims prepared source and the checks stated
above, not a prospective Jenkins pass or activation outcome.

Ops artifact correction: native build20 passed all three full-main gates and both
fixed artifact build commands, then failed export safely because Cargo's normal
release/deps hardlink was rejected by the generic single-link reader. Preserve
that failed receipt and its diagnostics. The fixed-host-only reader now admits
exactly the same-inode owned Cargo pair, pins both descriptors and rechecks
metadata after reading; authority and asset inputs retain the single-link rule.
A real tiny offline Cargo release regression verifies the normal two-link input
and independent immutable one-link export, with hostile link and mutation cases.

--- Previous task record ---

# Current: Coordinator-owned post-merge verification — 2026-11-23

Completed source implementation. The existing coordinator now claims transactional
post-merge verification actions for confirmed native and external merges. Ready
factory-selected Jenkins v2 ports verify the actual merged commit/tree. Frozen
requests, leased checkpoints, uncertain-ack reconciliation, deadline admission,
correlated cleanup and cleanup-only recovery after revoked permission prevent
new duplicate work while preserving completed author/reviewer history.

Final code f1b8b3b has aggregate verification closure in
/mnt/data/homelab-build/post-merge-coordinator-20261123/verification-closure.json.
Clean `85303c1` passed 130 preflight tests and branch collection of 1,011 tests: 704 executed,
307 declared PostgreSQL not-run, 0 unexpected skips/failures. Main ran services 6,
driver 2 and all 1,011 runner tests with 0 skips; 17 failure/subtest entries were all
stale current-schema/read-fixture expectations in exactly 4 test files. Preserve
that failed receipt. The only code delta after this full collection is the
reviewed 4-file fixture correction (11 additions/7 deletions), no application/SQL
changes. Fresh f1b8b3b verification passed 9 source guards, all 36 affected database
tests and remaining 3 Studio tests, zero skips/failures. This covers 1,022 distinct
backend tests across original and affected scopes; it is not a new one-shot
full-suite success claim. Native Rust/WASM/browser evidence is reused only after
433 tracked native/cooker and 91 embedded inputs match aae81e4 byte-for-byte.

Source publication targets main. No live schema 28, worker grant, v2 helper/profile,
configuration, services or portal activation was performed. Failed-main results
create separate durable remediation evidence; automatic admission of another
coding task is outside this handler and must use current WorkService policy.

Integration checkpoint: provider, fair coordinator scheduling, additive M28
transactional native/external merge hooks, exact-main request/receipt fencing and
cleanup-only recovery after permission revocation are integrated. Current source
009e56f passes130 early checks with zero skips; component validation includes
12 actual PostgreSQL cases after final defenses,26 policy-upgrade cases before
the final two narrow defenses, and42 existing native/external workflow cases.
All27 historical migrations and20 legacy authority method ASTs are unchanged.

Failed checks create independent durable remediation evidence; automatically
admitting another coding task remains outside this handler and must use current
WorkService policy. Completed parent tasks are not reopened. Native evidence
reuse is explicit:433 tracked Rust/cooker files and91 literal embedded inputs
match main aae81e4; prior Rust/WASM/browser execution keeps original provenance.
Fresh branch and four-suite main backend checks follow against this clean tree.
Evidence is under /mnt/data/homelab-build/post-merge-coordinator-20261123.
No live schema, configuration, Jenkins profile or portal activation has occurred.

Implement the approved missing handler inside the existing coordinator. PostgreSQL
owns durable queue/state and wakeups; the coordinator selects/injects ready ports
through factories and invokes the existing Rust Jenkins IoClient. No new service,
broker, runtime registry or model publication credentials. Persist confirmed
merge receipt and follow-up action atomically; bind the actual merged/main commit,
not merely the authored candidate. Recover durable build intent on restart and
reconcile uncertain submission before any new write. Successful premerge work
remains immutable; a failed main check is new follow-up work, not a rewritten
historical success. Legacy v1 workflows stay valid; new main checks require v2.

Root owns coordinator integration, config, review, final verification/publication.
Store agent owns additive SQL/DAO contract and actual DB tests. Provider agent
owns scoped Jenkins preparation/reconciliation adapter. Tests/docs agent owns
focused workflow regressions and documented component/sequence views after the
shared contract is agreed. Each writer uses an isolated worktree. Preserve all
existing worktrees, untracked data, historical migrations and protected receipts.
This task implements source; no live schema/config/profile/service activation,
model task, GitHub Actions run or automatic portal deployment is assumed.

Provider handoff (`jenkins_main_run`): added the coordinator-owned v2 main
verification adapter and its private Git/Jenkins tests. The frozen request binds
the actual merge commit/tree, uses the durable post-merge ID as the verify action,
and derives its prepare ID with UUIDv5. It does not reopen a review lease or
rewrite/push source. Existing trusted delivery owns the spool, uncertain-submit
journal, exact result validation and confirmed cancellation. The effect-free
factory selects the ready Rust-backed delivery only after validating private
configuration against owner, repository, canonical source and GitHub scope.
Focused provider/existing-delivery/v1-v2 consumer tests: 33 passed, zero skips,
4.005s; `git diff --check` passed. No live operations or broad build was run.

Tests/docs handoff (`portal_release_ready`): added17 focused worker/config/factory
regressions, including durable checkpoint-before-submit, same-request restart,
source/identity/lease rejection, full v2 main receipt validation, pending cleanup,
cancellation recovery and disabled-provider zero construction. The invalid UUID
outcome test exposed an admission bug; Root fixed it before the final run. All50
new/provider/legacy publication/config/merge tests passed in7.662s, zero skips.
The new architecture source contains editable ownership and sequence diagrams,
and the CI guide now distinguishes implemented scheduling from unactivated
runtime configuration. Scoped local links and both diagrams' official Mermaid
grammar passed. No Rust/browser/Jenkins/live execution or portal page change.

## Store component checkpoint

Owner: `latest_source_audit`, isolated `store` worktree, base `aae81e4`.
Added M28 future-only native/external merge signals, scoped durable verification
queue/leases/frozen request, DB-time start/deadline fences and separate failure
evidence. A revoked start grant retains only the same frozen action role's
cleanup/recovery; startup recognizes that cleanup, and cancelled actions cannot
pass. Existing authorized Muse read defaults remain explicit read-only; no
historical SQL1–27, original merge/review authority, roles, credentials, HBA or
completed task receipts are rewritten. No live schema/config/service effects.

Verification: 38 actual PostgreSQL16 regressions passed with zero skips (12 new
queue/transaction/receipt/ACL cases plus all26 policy-upgrade checks). After the
final cancellation/startup defenses, reran all12 new actual-PG cases: passed,
zero skips,32.295s. Full strict current catalog verification passed on fresh,
reapply,26→28 and completed27→28 paths; the last preserves completed root rows
and the exact original M16 merge_finish definition with no backfill. Evidence is
outside Git under `post-merge-coordinator-20261123/post-merge-store-*.log`.
Root owns coupled M27 context/inventory qualifications and overall CI/publication.
A failed-main remediation row is durable independent work evidence; automatic
model admission is not granted by this store component and needs explicit
current-policy admission by the coordinator.

--- Previous task record ---

# Current: Fix repository review findings — 2026-11-23

Completed all five review findings: explicit compatible CI profile contracts,
all four main backend suites, source-aware core layer enforcement, early
qualification, and current documentation/examples. Final code f3c2de0 passed
98 preflight tests, 34 JS harness tests, all 79 host tests, all 28 real browser
acceptance groups with cleanup, and 978 backend tests (6 services, 2 driver,
967 runner, 3 Studio), zero outer failures/errors/skips. The actual branch run
also passed with 672 executed and 295 declared database cases explicitly not-run.

Original `8376d66` passed the full Rust feature/debug/release matrix, lint, docs and
WASM/generated-asset checks. Its overall main attempt failed an obsolete browser
assertion; both failed browser receipts remain preserved. Fresh final checks
cover the corrected browser assertions, pin and embedded-document formatting.
All 262 Rust files and 25 Cargo manifests/locks remain byte-identical; the refreshed
host was rebuilt/retested and all six generated browser assets plus the agent
match prior fingerprints. Reuse retains original execution attribution.

Evidence: /mnt/data/homelab-build/review-fixes-20261123/{integrated-remaining-r2.json,
integrated-branch-r2.json,native-input-identity.json,component-integration-review.json}.
Final checks ran locally, not in a new Jenkins build. This completion record is
the only post-verification change. Source publication targets main; no live
helper/profile/configuration/database/portal activation occurred. Last qualified
live release remains `95651a5`; the scope-aware profile needs separate activation.

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

Integrated source qualification: all104 historical raw paths resolved; two reviewed current-guide values and eleven explicit CI source-context overrides match actual bytes. Historical v1 baseline hashes remain literal provenance; all26 migrations and20 legacy authority ASTs pass unchanged. Full9-case M27 qualification passed0.626s before expensive checks. Actual branch-scope and full-main verification follow on this integrated revision.

Actual branch attempt1 found missing installed contract and inherited PostgreSQL skip provenance; follow-up4bed406 adds required module publication and exact MRO declaration matching.21 focused packaging/runner/contract tests passed2.770s with no outer skips. Preserve original966-test branch failure; rerun on corrected source.

Browser follow-up: Root integration passed Rust270/WASM/generated-JS checks, then the implementation reader journey found obsolete documentation assertions. Updated only implementationDesignJourney to verify the historical Mail profile label, current navigation/ABI13/schema26, scoped last-qualified956/builta3/499-input provenance and non-enabled Studio/write flags. Preserved the three-table, same-tab/back, viewport-overflow, script-absence and diagram-load checks. Node syntax check and all34 existing QA harness tests pass with zero skips. No production JS, Rust build or full browser run here; Root reruns actual browser acceptance on final integrated source and qualifies the changed journey hash.

Rendered-doc follow-up: the first renewed browser run exposed literal Markdown emphasis because the navigation bold span crossed a physical source line. Kept that span on one line; fetched actual fixture HTML confirmed the cause, and the current reader assertions now pass against a sandboxed Chrome page. Rebuild and retest the host for its changed embedded document; unchanged Rust/UI/WASM checks retain original8376 attribution. Preserve both prior failed browser attempts and rerun full browser plus all four backend suites before publication.

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
