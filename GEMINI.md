# Forge IDE coding context

Your purpose in this workspace is to improve Forge IDE and its homelab framework:
generated Chat, Mail, Workbench and Design, durable PostgreSQL state and queued
inference, and the supported game tools. The active user request sets the scope;
this purpose is not an instruction to implement every proposal in the repository.

## Start with the real environment

The canonical checkout is `/home/lrocha/repos/homelab` on Mission Control. Verify
the actual workspace/branch when using a linked worktree. Before work read:

1. `/home/lrocha/repos/AGENTS.md` and `/home/lrocha/repos/mission-control/AGENTS.md`.
   Antigravity discovery stops at the Git root, so these parent instructions must
   be read explicitly. They define host boundaries and retired/protected projects.
2. This repository's `AGENTS.md` and `.agents/skills/wire-forge-providers/SKILL.md`.
3. `README.md`, `docs/design/implementation-status.md`, then task-relevant source,
   crate README files and contracts. Read nested instructions before editing there.
4. For execution ownership, `docs/architecture/proposals/forge-worker-ownership.md`.

Check `git status --short --branch` before edits. Preserve unrelated changes and
linked worktrees. Use an isolated worktree for implementation when appropriate.
Existing skills are discovered directly from `.agents/skills/`; keep those files
as the authority instead of copying their instructions into a second skill set.

## Preserve Forge's architecture

Start capability, adapter and dependency work with Wire Forge Providers. Bootstrap
selects implementations and scopes through typed configuration and effect-free
registrars, validates/freezes the startup plan, then creates and injects factories,
ready ports or scoped providers. Consumers and workers do not look up global
registries. Pure values, reducers and contract-only crates need no empty registrar.

Trace typed events and effects through the single admitted, header-routed gateway
and independently authorized host/agent. Preserve bounds, owner/session/report/
request/revision identities, stale-result rejection, cancellation and cleanup.
Use the actual DAO/DTO and storage contracts; do not infer ownership from UI labels.

Application HTML/CSS/JS are generated from typed Rust components and YAML
style/theme declarations. Do not hand-edit generated assets or hide application
markup, scripts or CSS in raw Rust/template strings. Read Design Modern UI,
Design Forge UI and the relevant UX skill before changing pages, and Test Forge UI
before claiming the rendered behavior works.

Use actual ownership: IDE application state is in worker-hosted Rust/WASM; main JS
owns DOM/presentation/lifecycle and generated worker glue performs admitted I/O.
Playtest instead uses page-hosted Rust/WASM with an I/O-only JS worker. Studio's
legacy generated JS controller is a documented remaining exception. Do not invent
one universal thread layout or claim a proposed Rust Studio controller exists.

## Work through the SDLC

Read current source and contracts, report evidence and uncertainty, and form a
bounded plan before implementation. When the user has already authorized changes,
carry them through implementation and verification without asking for routine
choices again. A setup or review request alone does not authorize unrelated fixes.
Use relevant skills for diagnosis, package boundaries, command tracing, UML and UI.
Keep design/status documentation aligned when behavior or ownership changes.

Use Context7 MCP for current library/platform/CLI documentation: resolve the
library first, then query the selected ID. Send documentation questions, never
credentials or private source. If the tool is unavailable, report that limitation
and verify against official documentation; do not invent APIs from memory.

Put build products, reports and temporary databases outside source under
`/mnt/data/homelab-build`, using a task-specific directory. Use the root Makefile
and documented prerequisites. Detect the installed Rust version before running
checks: the local stable toolchain and GitHub's current stable may differ.
`CARGO_TARGET_DIR` should point outside source. Run focused tests and required
full gates appropriate to the change; code/UI acceptance uses `make ci` and
`make ui-qa` (all 28 ordered browser groups). Runner SQL tests use disposable,
socket-only PostgreSQL, never production setup. Documentation/configuration-only
changes need relevant configuration/link/discovery checks, not invented test passes.

Invoke verification-before-completion when preparing a completion claim. Report
exact source/revision, commands, results, failed/skipped coverage, and publication
and deployment state separately. Existing `git` and `gh` can inspect repository
history and CI. Commit, push, merge, deployment and live service changes must
follow the user's actual task authorization; configuring this agent grants no
blanket publication or production access.

Keep `.gem`, OAuth files, keys, real messages, model weights, production databases
and retired/unrelated repositories out of context packets and reports. Preserve
SSH and Cloudflare access. Use the verified mount/SSH instructions before any
explicitly requested Forge GPU work. Do not install container runtimes as setup.

## Shared work and quota handoffs

Follow `AGENT_WORK.md` for the current task, authorized scope, ownership, evidence
and exact next action. You are the preferred implementation worker; Codex or the
named coordinator reviews and handles takeover when included quota is exhausted.
Keep one writer per worktree. Checkpoint before changes, after coherent changes,
and before/after expensive checks. A hard quota stop may prevent a final message.
Stop on quota exhaustion rather than retrying in a loop or enabling paid overages.
The coordinator handles switching; do not claim automatic quota detection exists.

Reuse the current conversation and loaded context. After initial orientation,
read changed/relevant files instead of repeatedly scanning the whole repository.
Keep status updates and handoffs concise. Preserve required full gates and reuse
verified evidence on byte-identical artifacts rather than rerunning it per agent.
When a review reveals a reusable verified lesson, update the relevant existing
skill, contract or regression test within the authorized task. Do not self-assign
deferred proposals just to consume remaining quota.

## Handoff recorded on 2026-11-19; recheck before acting

- Corrected Gemini App S2 database-verification and B2 Mail-wording changes are
  complete on main at `2e1300a00da92e9f97bdb1a656c2b182d4219a6c`. The Drive input
  snapshot was refreshed. Do not repeat the old completed task.
- Hosted Rust CI had a pre-existing Rust 1.100 unreachable-call lint in
  `src/rust/forge-remote-https/examples/loopback.rs`; local verification used Rust 1.97.1.
- Hosted browser acceptance failed draft restoration at `src/rust/forge-ide-web/tests/browser/journey.mjs:1497` on
  the initial run and a retry. A draft-binding race and the existing
  `qa/durable-draft-wait` branch are leads, not a proven fix or merge permission.
- Exact results are in `/mnt/data/homelab-build/gemini-drive-apply-20261119/validation.json`
  and the scoped hosted CI investigation JSON files beside it. These are supplied
  historical results, not checks you have freshly run.
- Manual Mail archive/ABI 5 support exists in source but the prior task did not
  deploy it. Deployment compatibility and live enablement must be checked separately.
- Multi-tab retained-report/cache work, Studio admission/provider refactors and
  broader Studio Rust/WASM work remain deferred until included in an active task.
