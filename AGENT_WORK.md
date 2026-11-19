# Shared Agent Work Checkpoint

- **task_id:** `replace-mail-with-agents`
- **objective:** Replace Mail with Agents in the portal navigation and remove the active Mail UI while preserving PostgreSQL history and inert legacy local drafts.
- **authorized_scope:** User requested Mail removal/replacement. Existing commit, push and portal deployment authorization persists. Preserve stored Mail data, private backend contracts, agent task receipts, account settings and unrelated services. Disable public Mail admission in the deployed web configuration; no database migration or API/worker restart is needed.
- **phase:** `verifying`
- **coordinator:** Codex
- **worktree/base:** `/home/lrocha/repos/homelab` / `c389f02`
- **ownership:** Root owns generated application/transport/styles, cache-forwarding compatibility, source docs and integration. Model author owns Rust state/projection/legacy restore tests in a separate worktree. Native author owns shared Home/Design navigation, legacy HTTP redirects and native tests in a separate worktree. Browser author owns the journey and its guide in a separate worktree.
- **decisions:** Five global tabs: Chat, Agents, Workbench, Home, Design. Agents remains available as an honest unconfigured page when Work/Usage is absent. Legacy /mail and #mail lead to Agents; decoded old Mail actions cannot execute. Preserve legacy local draft hints inert with active=false and no replay/locks. Remove Mail components, transport bindings/timer and scoped styles. Keep private database/backends/history and dated design references.
- **verification:** All isolated patches integrated. Strict Clippy and 23 generator tests passed. Full fixed make ci and 28-group sandboxed browser gate pending; source frozen for verification.
- **publication_deployment:** Current live release aa2eb1a remains unchanged. Latest main includes the reviewed Gemini merge c389f02. Prepared rollout will change only the web host/config/assets and preserve backend workers/credentials/data.
- **evidence:** `/mnt/data/homelab-build/mail-to-agents-20261119/`
- **next_action:** Complete full native/browser gates, publish the tested source, and verify the web-only deployment.
- **last_updated:** 2026-11-19T23:29:57.729864+00:00
