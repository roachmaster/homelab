# Mission Control PostgreSQL files

This directory holds the SQL for the Forge agent database on Mission Control.

| Path | Purpose |
| --- | --- |
| `provision.sql` | Create the agent database and its owner and runtime roles. |
| `apply.sql` | Apply registered migrations in order and record their versions. |
| `migrations/` | Store numbered schema changes. |
| `management/grant-runner.sql` | Grant the runner access to the six application tables. |
| `management/verify.sql` | Check the expected schema and show the applied versions. |
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
