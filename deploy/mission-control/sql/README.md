# Mission Control PostgreSQL files

This directory holds the SQL for the Forge agent database on Mission Control.

| Path | Purpose |
| --- | --- |
| `provision.sql` | Create the agent database and its owner and runtime roles. |
| `apply.sql` | Apply registered migrations in order and record their versions. |
| `migrations/` | Store numbered schema changes. |
| `management/grant-runner.sql` | Grant the runner access to its scoped application tables and grant the GPU worker access to its function API. |
| `management/verify.sql` | Check the expected schema, migration 0008 archive constraints, migration 0009 coding admission constraints/PUBLIC revocation, GPU worker privileges, and applied versions. |
| `management/verify-runner-privileges.sql` | Check required legacy permissions and exact effective permissions on the scoped Mail, GPU, and coding-admission runtime tables (used by `setup-postgres.sh --check` and disposable database tests). |
| `management/grant-muse-read-access.sql` | Explicitly grant the existing Muse account its reviewed read-only capability; never creates or changes the account. |
| `management/verify-muse-read-access.sql` | Read-only exact optional Muse reader contract, included by schema verification. |
| `management/run-status.sql` | Summarize run and tool states without printing prompts or tool output. |
| `tests/schema.sql` | Exercise the schema with a disposable transcript inside a rolled-back transaction. |

## Apply and inspect

Use the [setup script](../setup-postgres.sh) to provision the cluster and apply
migrations as `forge_agent_owner`. Connect as the `forge_agent` operating
system account to inspect run status through peer authentication.

```bash
sudo deploy/mission-control/setup-postgres.sh --check
sudo -u forge_agent psql -X --host=/var/run/postgresql --dbname=forge_agent \
  --file=deploy/mission-control/sql/management/run-status.sql
```

Run `apply.sql` with the database role that owns the schema. It takes a transaction-scoped lock, applies pending migrations, and commits their version records with the schema changes. For the next schema change, add the next numbered file and register it in `apply.sql` and `management/verify.sql`. Once a migration has been applied, write a new migration for any correction.

The verification scripts are read-only checks; they do not repair grants or
rewrite applied migrations. The runner check retains the existing legacy
permission checks and enforces the intended permissions on `mail_threads`,
`mail_messages`, `inference_results`, `gpu_worker_readiness`, `coding_tasks`,
`coding_events`, and `coding_control_receipts`. Required access
and unexpected effective access are checked separately for these scoped tables.

Migration `0009_coding_admission.sql` adds the three core durable admission
tables (`coding_tasks`, `coding_events`, `coding_control_receipts`) in
`forge_agent` with `PUBLIC` revoked and exact `forge_agent` grants (`SELECT`,
`INSERT`, `UPDATE` on `coding_tasks`; `SELECT`, `INSERT` on `coding_events` and
`coding_control_receipts`). Separate coding-worker schemas, roles, and SQL
functions are intentionally deferred until the execution-worker slice. This
migration is registered in `apply.sql` for verification in disposable PostgreSQL
16 tests and is not deployed to production by repository changes alone.

Migration 0008 verification checks both archive columns and their validated
CHECK constraints, including the existing revision bound. Canonical PostgreSQL
16 expression tokens are compared without executing stored constraint code;
whitespace outside quoted values is insignificant. An unexpected definition
fails closed and requires review, even when its author intended an equivalent
expression.

The existing runner integration fixture exercises these checks on a disposable,
socket-only PostgreSQL 16 cluster. From `deploy/mission-control/agent-runner`, run:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest -v test_workspace_store
```

This needs the runner's Python dependencies, PostgreSQL 16 binaries, and a
non-root test user. A skipped database suite does not establish that verification
passed. Drift cases change only the owned fixture and restore its schema and
permissions; the deployment setup script is not the test harness.

## First schema

- `conversations` identifies the owner and chat. `runs` records the selected model and job state. A request ID is unique within its conversation.
- `messages` records transcript order and links every entry to a run in the same conversation. An assistant message can reference the original tool call. A tool message references its recorded result.
- `tool_calls` and `tool_results` correlate a sandbox result to its original call. A call can record an unknown outcome after an interruption.
- `run_events` stores progress events in sequence for reconnects and replay in the IDE.

The runner will claim tool calls, enforce sandbox limits, and decide how an interrupted run is shown. Database migrations manage schema versions. The service lifecycle script manages native services.

To exercise the constraints on a disposable database:

```bash
psql -X --dbname=forge_agent_test --file=deploy/mission-control/sql/apply.sql
psql -X --dbname=forge_agent_test --file=deploy/mission-control/sql/tests/schema.sql
```

## Optional Muse read access

The externally managed `muse_events` login can read the coding lifecycle without
owning or executing it. When that role exists, verification requires direct
`CONNECT` on `forge_agent`, `USAGE` on its `forge_agent` schema and exactly
non-grantable `SELECT` on these thirteen tables:

- `coding_events`, `coding_plan_decisions`, `coding_result_chunks`
- `coding_reconciliation_receipts`, `coding_publications`, `coding_hosted_checks`
- `coding_feedback`, `coding_reviews`, `coding_tasks`, `coding_attempts`
- `coding_workflows`, `coding_workflow_reviews`, `coding_workflow_actions`

The check rejects extra table or column grants, writes, grant options, API or
function execution, ownership, elevated role flags, memberships and default
privileges that extend this scope. The existing owner/schema-scoped table
default is the sole exception: `forge_agent_owner` in schema `forge_agent`
grants non-grantable `SELECT` on future tables to `muse_events`. Its exact
catalog row is required and retained, not replaced by a global grant. New
tables still require an explicit update to the current-table allowlist before
strict verification admits them. Existing `INHERIT` or `NOINHERIT` configuration
is retained; neither adds authority without memberships. Historical catalog
snapshots accept only the exact owner-issued Muse read ACL while preserving all
other fields and ACL entries. Missing required reads fail verification without
repairing or revoking permissions.

On hosts where an administrator has already provisioned this account, the
explicit management command is:

```bash
sudo -u postgres psql -X --dbname=forge_agent \
  --file=deploy/mission-control/sql/management/grant-muse-read-access.sql
```

This command is transactional and repeatable. It refuses an absent account and
never creates a login, alters passwords or role attributes, or rewrites HBA.
The standard schema path creates no Muse account and keeps migrations1–19 and
P10 unchanged. The source HBA include retains the dedicated localhost SCRAM rule
for `muse_events` before its general connection rejections; no public listener
or password authentication fallback is introduced.

Read access does not enable external intake, task submission, approval,
execution, reconciliation or merge. The [external intake design](../../../docs/architecture/muse-agent.md)
remains dormant and separate from this capability.
