# Current Homelab handoff

Codex owns the Rust deployment cutoff on `feature/codex`. Reviewed main
`6fa2e13d` is the last verified live baseline (schema 54, portal ABI 19).
The feature branch contains the Rust Jenkins verifier, runner, release actor,
PostgreSQL proxy, external delivery and GPU inference worker. Deployment Python
has been removed from the candidate. Source qualification is in progress.

Before rollout, verify a native Plan can advance through independent review
and implementation. The current Rust coordinator takes a second project lock
during a workflow decision. Qualify the integrated fix through branch and main
CI, take a fresh database backup, drain the runner queue, and activate the
matched runner, publisher, verifier and release actor together.

Migrations 56 and 57 passed rehearsal on backup data. The optional proxy
transport switch (55) and the digest-column drop have separate cutovers.
Preserve applied migrations, historical receipts, budgets and the unresolved
rollback journal for `4d8ee580`. Branch backend tests skip disposable PostgreSQL.
Main runs the database and real-browser suites.

Send the task UUID, outcome and next action. Connected agents retrieve details
through MCP. Sandboxed workers use their staged artifacts.
Read the relevant section of the [checkpoint archive](AGENT_WORK_HISTORY.md)
when tracing an earlier decision or looking up a receipt.
