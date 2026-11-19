# Shared Agent Work Checkpoint

Compact shared handoff record for **Antigravity** (primary worker using included subscription quota), **Codex**, or another named authorized agent (coordinator/reviewer for reviewed takeover when quota is low or exhausted). This is a manual shared-record workflow, **not** automatic quota detection or agent switching.

## Operating Rules

- **Single writer:** One active writer per worktree; explicit user scope and acceptance criteria govern all edits.
- **Checkpoint cadence:** Update this file before changes, after coherent changes, before and after expensive checks, and before handoff. Hard quota stops may prevent a final note, so checkpoint early.
- **Quota discipline:** Never retry quota failures in a loop or enable automatic purchased-credit spending; stop and hand off cleanly.
- **Takeover protocol:** Incoming agents must first compare `git status`, `git diff`, branch/HEAD, and active processes against this checkpoint, then claim ownership.
- **Verification reuse:** Reuse prior evidence only after confirming identical source; changed source requires relevant reruns. Required Forge gates (`make ci`, `make ui-qa`, etc.) are preserved where applicable, but do not re-run identical full gates across agents on unchanged artifacts.
- **Forge conventions:** Routine coding preserves the factory/provider entry point (`wire-forge-providers` in `AGENTS.md`) and generated UI / worker rules in `AGENTS.md` and `GEMINI.md`. Capture reusable verified corrections in the relevant existing skill or contract instead of repeating chat history.
- **Scope hygiene:** Known CI failures are candidates for a next task, not assigned fixes here.

## Current Record

- **task_id:** `portal-agent-design-and-backend`
- **objective:** Keep coding agents free of inherited llama.cpp token caps and design portal visibility across Codex, Gemini/Antigravity and future providers.
- **authorized_scope:** User authorized publishing both agent designs to the live portal, then commit/push, then begin backend implementation. Antigravity first owns only the assigned generated Rust design readers/routes/tests; Codex reviews, stages an ABI-compatible docs release, publishes and pushes. Real agent execution/live DB provisioning and account spending are not part of this initial implementation slice.
- **acceptance_criteria:** Source-grounded distinction between inference caps and coding-agent policy; provider capability/provenance matrix; responsive Agents view; injected factories/providers, admitted DTO/DAO contracts, accurate accounting and collection sequence; validated diagrams and links.
- **phase:** `implementing`
- **active_agent:** `Codex (canonical UI); Antigravity (isolated Python worktree)`
- **coordinator:** `Codex`
- **worktree:** `/home/lrocha/repos/homelab`
- **branch:** `main`
- **base:** `2e1300a00da92e9f97bdb1a656c2b182d4219a6c`
- **HEAD:** `624409a70f7d4d45aaccb768bc581d31f8eb66fb`
- **changed_files:**
  - Task: `docs/architecture/proposals/forge-agent-usage.md`, `docs/architecture/proposals/forge-coding-agent-dispatch.md`, `docs/README.md`, `docs/architecture/README.md`, `AGENT_WORK.md`.
  - Pre-existing local setup preserved byte-for-byte: `AGENTS.md`, `GEMINI.md`. Prior dispatch/checkpoint snapshots are retained in the external evidence directory.
- **decisions:** Coding recipes default to `generation_token_limit: null`; provider limits remain native. Adapter resource profiles and chunked/full-result pagination replace global short-output examples. Proposed Agents view separates account allowance/reset, observed-job usage, account activity, context and cost estimates. Scoped usage adapters receive injected ports, not registries. Canonical feeds, verified account epochs, dedup identities, trusted counter reset baselines, child lineage and ordered polling prevent double counting. Gemini headless allowance remains unknown until a supported collection feed is verified.
- **authoring_and_review:** Antigravity authored the new usage proposal using the signed-in included plan; its headless invocation returned SUCCESS with a nonempty response and empty stderr. Codex applied final contract corrections and indexes. Independent local-limit/provider-capability reviews and final storage rereview found no material design blockers.
- **verification:**
  - Six new Mermaid sources parse/render with official Mermaid 12.2.0, Node 24.15.0 and mmdr 0.3.1; all 44 gallery entries validated and rendered. New diagrams visually inspected.
  - Design document check: 111 local links/anchors and three JSON examples passed.
  - Canonical full `make ci` passed on Rust 1.97.1. A later browser-journey-only change updated Home from five to seven doc links and asserted the two exact new destinations. Native build inputs remained byte-identical; fresh `make ui-qa-check` and full `make ui-qa` passed all 28 groups.
  - Captured-live ABI-4 candidate passed fresh full `make ci`, all 28 browser groups, and release build. Its only non-design source differences were the intended Home-link assertions and a Rustdoc comment correction required by strict rustdoc.
  - Staged and live sandboxed Chromium checks passed for both readers on phone/desktop, all six diagrams, same-tab navigation and overflow. Home and reader screenshots visually reviewed.
  - Live publication verified fixed routes/MIME/CSP, preserved active ABI-4 UI assets byte-for-byte, unchanged peer services and Cloudflare Access sign-in enforcement.
- **evidence:** `/mnt/data/homelab-build/portal-agent-design-20261119/` (native/browser reports, exact-source reuse proof, deployment candidate/stage/publication evidence); design-authoring evidence remains under `/mnt/data/homelab-build/agent-usage-design-20261119/`.
- **running_processes:** Table-fix staging service stopped after publication. Antigravity is authoring Python/SQL in its isolated worktree; Rust admission work is frozen in a separate worktree. No coding/usage worker, reconciler or collector installed or started.
- **publication_deployment:** Designs pushed to main as `624409a`. Both readers are live at `/design/coding-agents` and `/design/agent-usage`. User-requested mobile table correction published in release `20261119T172723Z-624409a-e17aa8ad-table-readability`: minimum column widths, normal prose wrapping, focusable local scroll regions and phone swipe cues. This remains a docs-only overlay preserving the live ABI-4 application bundle; no backend activation or full-main/ABI-5 deployment.
- **table_fix_verification:** Full canonical CI passed on Rust1.97.1; native inputs reused after proven removal of a task-created CPython bytecode file (no source/build/test changes). First browser run passed all28 groups but failed source-consistency after that cleanup; a fresh unchanged-source run passed28/28 with cleanup. Captured-live candidate independently passed full CI, all28 browser groups and release build. Its dependency receipt was rebound to the fresh canonical run with preserved failure/provenance, without rewriting QA artifacts or rerunning candidate checks. Staged/live sandboxed Chrome verified nine comparison regions at320/390/1440, readable cells, normal prose wrapping, visible mobile cues, focus/actual arrow-key scrolling and last-column access. First/last-column and cue screenshots reviewed. Skill validation passed.
- **table_fix_evidence:** `/mnt/data/homelab-build/portal-table-fix-20261119/` (native reuse proof, freshQApath, candidate checks/receipt provenance, staging/live screenshots and publication evidence).
- **hosted_ci:** Design commit retains known pre-existing Rust1.100 unreachable-call failure in loopback.rs and draft-restoration browser failure at journey.mjs:1497. Local gates passed on Rust1.97.1; hosted CI is not claimed green. These fixes are outside the current slice.

- **last_updated:** `2026-11-19T17:30:43.568803+00:00`
- **observed_quota:** `unknown`; no purchased-credit fallback enabled. Native token counters are observations, not a subscription balance or invoice.
- **blockers_uncertainties:** Actual provider account responses, supported headless Gemini allowance feeds and write-profile process isolation remain unverified. Preserve the design's opt-in activation gates.
- **next_action:** Designs pushed to main624409a. Antigravity owns Python/SQL admission only (submit/get/list/events/queued cancel) in `/mnt/data/homelab-build/coding-admission-20261119/python-worktree`. A separate Rust agent owns the independent relay/config in `/mnt/data/homelab-build/coding-admission-20261119/rust-worktree`; coordinator integrates after both freeze. Frozen contract and brief are under `/mnt/data/homelab-build/coding-admission-20261119/`. Workers, attempts/results/resume/accept, usage polling/UI and live backend activation remain subsequent slices.

## Blank Task Template

Replace `Current Record` with these fields when starting a new task:

```yaml
task_id: ""
objective: ""
acceptance_criteria: ""
authorized_scope: ""
phase: "planned | implementing | verifying | ready_for_review | handed_off | done | blocked"
active_agent: "none | Antigravity | Codex | <named-agent>"
coordinator: "Codex | <named-agent>"
worktree: ""
branch: ""
base: ""
HEAD: ""
changed_files:
  task_changes: []
  pre_existing: []
decisions: []
verification:
  source_identity: ""
  checks_run: []          # exact command -> exit/result, or "not run (<reason>)"
running_processes: []     # observed only, or "none started / host unknown"
publication_deployment: "uncommitted | committed | pushed | merged | deployed"
last_updated: "YYYY-MM-DDTHH:MM:SSZ"
observed_quota: "unknown | <observed value> (timestamp)"
blockers_uncertainties: []
next_action: ""
```
