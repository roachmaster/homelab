## Agent branch ownership — user instruction, 2026-11-27

All Homelab agents must develop, edit, commit and push only on their assigned
persistent feature branch:

| Agent | Assigned branch |
|---|---|
| Codex — primary orchestrator, including its delegated helpers | `feature/codex` |
| Gemini / Antigravity | `feature/agy` |
| Dot — separate ChatGPT agent | `feature/dot` |
| Grok | `feature/grok` |
| Muse | `feature/muse` |

Before editing, run `git status --short --branch` and `git branch --show-current`,
then verify the current branch matches your identity above. Inspecting another
branch is read-only. If you are on `main`, a detached HEAD, a historical task
branch or another agent's branch, preserve the existing checkout and use an
isolated worktree for your assigned branch before making changes. Do not switch
or reset a checkout containing someone else's work.

Fetch current refs and bring reviewed `origin/main` changes into your own feature
branch when needed. Keep your work and checkpoints on that feature branch; push
explicitly to its matching `origin/feature/<agent>` destination. Do not create a
replacement task branch, push to another agent's branch, force-push, delete or
rename agent branches without a new explicit user instruction.

Codex remains the orchestrator and follows the same feature-branch rule. Main
integration uses the reviewed, CI-verified merge workflow; agents must not bypass
it with direct pushes to `main`. Earlier one-off main-push instructions do not
provide a standing exception to this newer branch policy.

Delegated implementation belongs to the orchestrating agent's assigned branch.
Keep one writer per worktree and serialize branch publication. Read-only analysis
may inspect preserved snapshots; do not commit development on detached snapshots.
If a receipt-bound automated job is pinned to a different branch (including the
older `forge/agent/*` defaults), stop before editing or publishing and report the
mismatch to Codex. Preserve its frozen identities, receipts and worktree rather
than rewriting them to make the branch look compliant.

Read this policy and `AGENT_WORK.md` before every new task. This records branch
ownership; it does not grant new deployment, merge or paid-credit authority.

# Forge plug-and-play

Start with [Wire Forge Providers](.agents/skills/wire-forge-providers/SKILL.md)
before designing, implementing, or reviewing a Forge capability, module,
service, adapter, worker, or runtime dependency. Use specialist skills from
that entry point when the task calls for them.

In the normal application path, bootstrap selects the implementation and
scope through typed configuration. Stage effect-free registrars, validate and
freeze the startup registry, create the selected instances, and inject ready
ports. Inject a scoped provider where the consumer needs runtime acquisition.
Keep dependency selection at the composition root and keep pure logic and
contract types independent of provisioning.

Before changing a page, layout, or frontend component, use
[Design Modern UI](.agents/skills/design-modern-ui/SKILL.md) to define its
visual hierarchy and interaction states. Follow
[Design Forge UI](.agents/skills/design-forge-ui/SKILL.md) for typed Rust/YAML
authoring and [Test Forge UI](.agents/skills/test-forge-ui/SKILL.md) to verify
the rendered result.

## Shared agent workflow

Use [AGENT_WORK.md](AGENT_WORK.md) as the active task, ownership and handoff record.
Antigravity is the preferred implementation worker while its included quota is
available; the coordinating agent assigns bounded work, reviews the actual diff,
and verifies delivery. Codex or another authorized agent can take over from the
same record. Use one writer per worktree; parallel writers need separate worktrees.
Delegate when it reduces total work, and handle small fixes or blockers directly
when that is more efficient. Ensure a new worktree has the current task record
and instructions; uncommitted setup files are not inherited by `git worktree`.

Checkpoint after coherent changes and before expensive checks so a hard quota
stop does not lose the next action. Record tests and source identity; reuse green
evidence only after confirming identical artifacts. Keep required quality gates.
Capture reusable verified corrections in the relevant skill, contract or test.
Before reporting that all agent work is published, inspect the accepted task
workspaces and relevant feature branches as well as the main checkout. Review
and integrate their changes, then verify remote reachability. Preserve
receipt-bound task workspaces by importing their snapshot into a separate
worktree.

The record coordinates existing authorization; it does not grant new publication,
deployment or paid-credit authority. Explicit configured AGY backup models may be selected by the coordinator after
a genuine published quota stop and stopped-source reconciliation. Original
workflow authority stays frozen; no paid-credit fallback or account polling is
authorized.


## Current portal views

The active portal navigation is Agents, Workbench, Home and Design. Chat is
temporarily inactive and Mail was replaced by Agents. Preserve their PostgreSQL
history and inert legacy local drafts, identities and pending-run hints. Legacy
Chat entry links resolve to Agents without Chat I/O. Reintroduce either UI only
when explicitly requested. Current UI
changes belong in typed Rust/YAML declarations and verified generated assets.
