# Optional PostgreSQL proxy

`forge-pg-proxy --config /etc/homelab-forge/forge-pg-proxy/config.json` is the
native replacement for the retired Python proxy. The source unit selects
`/opt/homelab-forge/event-current/bin/forge-pg-proxy`, inside the same immutable
release as the other event services. Installing its executable or unit does not
enable or start it. Migration55 is a separate, disabled production cutover;
this implementation does not apply migrations, alter grants, edit authentication,
or change direct PostgreSQL adapters.

Bootstrap validates `ProxyConfig`, stages `register_proxy`, freezes the startup
registry, then creates `ProxyFactory`. The ready `Proxy` implements `IoClient`
for authenticated operation requests. `ProxyServer` owns bounded HTTP admission,
worker threads and independent notification subscriptions. Shutdown closes
admission and wakes long-polls before joining workers and listeners. Configuration
selects explicit database/listen addresses and token-file paths; no client SQL,
role, owner or audit identity is accepted from an HTTP request.

The sample configuration retains six fixed caller roles. Token files must be
regular, singly linked, root/service-owned, not group-writable and have no other-user access. Group-read is allowed only
for the effective service group or one of its actual supplementary groups. Files
are opened nonblocking and checked before reading, so FIFOs are refused. They contain
32–256 ASCII token characters. Duplicate credentials are refused. Optional caller
`allowlist` entries can narrow each built-in permission set (`fn`, `op`, `read`,
`readq`, `notify`, `subscribe`); they cannot add permissions. The service binds
only loopback. Use the existing deployment authentication profile and keep the
proxy service disabled until separately qualified and authorized.

The neutral `src/proxy/catalog.json` is extracted without executing the retained
6fa2e13d server: 100 functions, 64 entrypoints,52 scoped tables, 81 fixed read
queries, 8 channels and 6 caller policies. Its provenance pins the original source.
Four catalog functions remain intentionally uncallable through every caller policy
(lock_attempt, queue_review, external artifact and capabilities). Column inventories come from the canonical disposable PostgreSQL schema. SQL is
fixed or assembled solely from this closed inventory; values remain bound
parameters. Omitted SQL-default arguments are omitted through named notation,
while explicit JSON null remains SQL NULL. Reads use read-only transactions and
mandatory owner/join/role scope. Fixed-query placeholders retain PostgreSQL contextual types, with
separate bindings for each occurrence; this preserves UUID values compared both
as UUID columns and JSON text. A mutation and its authenticated, redacted audit
record commit atomically; audit failure rolls the mutation back.

HTTP accepts `/health`, `/v1/operation` and `/v1/notifications`, rejects duplicate
JSON keys and Authorization headers, transfer encoding, oversized requests and
unknown fields. Requests are limited to1 MiB, responses16 MiB/10000 rows, generic
reads 1000 rows, notification waits 30 seconds, and worker admission is bounded.
No automatic operation retries occur. A lost commit acknowledgement is an
uncertain database result, requiring the normal idempotent caller protocol; an
error response does not fabricate rollback or acceptance. Notifications retain at most 1000 messages
per fixed channel, with independent cursors per authenticated caller; notification
publication and subscription are separate permissions. Database errors and logs
never return SQL, arguments or credentials.

The proxy intentionally uses short-lived request connections, avoiding pooled
owner/session leakage. Each transaction binds `forge.coding_owner` locally;
read-only and statement/lock timeouts are also local. No production performance
claim is made by disposable fixture tests.

The optional `db.password_file` explicitly selects a private libpq-format passfile
for a later password-authenticated cutover. It must be root/service-owned 0600,
singly linked, and supply exact selected role entries (including forge_pg_proxy).
Escaped colon/backslash fields are supported; wildcard host/port/database entries
may match, but wildcard role entries are refused. There is no implicit environment
or HOME password lookup. Preparation can omit this field and continue using its
existing ident rules. This does not execute or authorize migration 55.

Migration 57 repairs the independently reproduced migration 54 `ide_chat_insert`
text-to-date error with one explicit date cast. It checks the complete predecessor
body and security metadata, preserves function ownership/ACL, and has a complete
new-body/metadata/ACL verifier. No migration 1–56 bytes or historical data change.
