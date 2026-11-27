# Homelab MCP server (read-only PostgreSQL)

An MCP server that lets an agent run read-only queries against the Forge
PostgreSQL database on Mission Control. It is published at
`https://mcp.thedevex.com/mcp`. Cloudflare only routes traffic. The server
authenticates every request itself, using static bearer tokens.

```text
MCP client ──HTTPS──▶ Cloudflare (DNS + Tunnel, no Access app)
             ──tunnel──▶ cloudflared on Mission Control
             ──▶ 127.0.0.1:8791 homelab-mcp.service  (Host/Origin check, rate limit, bearer token)
             ──unix socket, peer auth──▶ PostgreSQL forge_agent as role homelab_mcp (SELECT only)
```

Status: **proposed**. This directory contains the code, unit and SQL. Nothing
is installed on Mission Control, no role exists in the database, and no
Cloudflare DNS record or tunnel route exists yet. The install steps below need
root and an operator's approval.

## Files

| File | Purpose |
|---|---|
| `mcp_server.py` | Server: MCP Streamable HTTP (JSON responses) at `POST /mcp`, plus `GET /healthz`. Python 3 stdlib + `psycopg2`. |
| `mcp_auth.py` | Static bearer-token check, token-file loader, failed-attempt limiter. |
| `mcp_oauth.py` | OAuth 2.1 authorization server (code + PKCE S256, refresh rotation, revocation), `DualAuth`, and a root-only CLI for clients, grants and the operator password. |
| `mcp_presence.py` | Optional authenticated heartbeat, bounded SQLite presence and private portal-reader authentication. |
| `sql_guard.py` | Defense-in-depth check that a query is one read-only statement. |
| `mcp_tokens.py` | Root-only CLI: `create`, `list`, `revoke`. |
| `homelab-mcp.service` | Hardened systemd unit. |
| `server.example.json` | Config template for `/etc/homelab-forge/mcp/server.json`. |
| `sql/grant-mcp-readonly.sql`, `sql/verify-mcp-readonly.sql` | Grant and verify the SELECT-only reader. |
| `test_mcp_server.py` | Unit, HTTP and disposable-PostgreSQL integration tests. |

## Tools

| Tool | What it does |
|---|---|
| `query` | Runs one `SELECT`, `WITH`, `VALUES`, `TABLE`, `EXPLAIN` (without ANALYZE) or `SHOW` statement and returns one page of rows. |
| `list_tables` | Lists the tables and views the role can read, with an optional `schema` filter. Paginated the same way as `query`. |
| `describe_table` | Lists the columns, types, nullability and defaults of one readable table. |

### Pagination

`query` and `list_tables` return a page object (as text and `structuredContent`):

```json
{"columns": ["id", "status"], "rows": [{"id": 1, "status": "done"}], "row_count": 100,
 "offset": 0, "page_size": 100, "has_more": true, "next_cursor": "eyJ2Ijo…", "truncated": true}
```

- `page_size` defaults to 100 and is capped at `max_rows` (500).
- When `has_more` is true, call the tool again with the **same arguments** plus `"cursor": "<next_cursor>"`. The last page has `next_cursor: null`.
- Add an `ORDER BY` on a unique key; without one PostgreSQL may return rows in a different order on each call.
- `SELECT`/`WITH`/`VALUES`/`TABLE` statements page through the unmodified statement with a server-side `DECLARE` cursor: `MOVE` skips to the offset and `page_size + 1` rows are fetched. No SQL is built around your statement. `EXPLAIN` and `SHOW` return one page only.
- Each page runs in its own read-only transaction, so pages reflect the data at the time of each call.
- Cursors are HMAC-signed with a per-process key and bound to the tool, the token, the exact SQL (or schema filter) and the page size. A tampered cursor, a cursor sent with different arguments or by another token, or one older than one hour (`cursor_ttl_seconds`) is rejected. Cursors stop working when the service restarts; start again without a cursor.
- If a page would exceed `max_response_bytes`, fewer rows are returned and `next_cursor` continues from the first row not returned. Offsets are capped at `max_offset` (1,000,000).

## Optional authenticated connection presence

Presence is disabled by default. Enabling it adds `heartbeat` for mapped,
authenticated `read` principals. Its annotations are **`readOnlyHint: false`**,
`destructiveHint: false` and `idempotentHint: true`: it changes only a local
last-seen timestamp. `query`, `list_tables` and `describe_table` remain read-only.
Presence never writes PostgreSQL, claims work or advertises execution readiness.

Add this optional object to the root-managed server configuration, replacing the
example IDs with actual static-token IDs or OAuth **client** IDs:

```json
{
  "presence": {
    "enabled": true,
    "store_path": "/var/lib/homelab-forge/mcp/presence.sqlite3",
    "reader_token_file": "/etc/homelab-forge/mcp/presence-reader.token",
    "subjects": {
      "static:0123456789ab": {"agent_id": "dot", "label": "Dot"},
      "oauth:hmcpc_0123456789abcdef": {"agent_id": "gemini", "label": "Gemini"}
    }
  }
}
```

Only administrators configure subjects. Token names, OAuth grant IDs and caller
labels cannot select an identity. Several credentials may map to the same agent
ID when labels agree. Up to 16 subjects/agents are accepted; IDs are lowercase
ASCII identifiers up to 64 characters and labels are printable text up to 80
characters/160 UTF-8 bytes. Keep Dot, Gemini, Grok, Muse and the primary Codex
orchestrator distinct from each other and from native execution profiles.

A mapped client sends this actual call every 60 seconds while connected:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"heartbeat","arguments":{}}}
```

The empty arguments object cannot contain an identity, label or timestamp. A
successful `structuredContent` is:

```json
{"agent_id":"dot","label":"Dot","state":"online","last_seen_at":"2026-11-27T12:00:00Z","heartbeat_interval_seconds":60,"freshness_seconds":180}
```

Times come from the server. A real observation stays `online` for 180 seconds,
then becomes `stale`; a mapped agent with no call is `never`. No background timer,
SQL query, tools listing, portal read or service restart creates a heartbeat.
Same-second calls coalesce. SQLite stores only agent IDs and server timestamps,
keyed by configured agent ID; token/OAuth grant rotation cannot grow the roster.
Removed agents are pruned at startup; retained agents keep their observations.

The existing systemd `StateDirectory=homelab-forge/mcp` supplies the service-owned
parent directory. It must already exist and not be writable by other users.
The SQLite file is private `0600`, limited to the 16-agent roster and a 1 MiB file
ceiling. It cannot reuse an existing authentication file or OAuth database.
When selected, unsafe/unavailable presence state or reader credentials fail
startup explicitly. No SQL migration or grant change is part of this feature.

The portal **host backend** reads `GET /internal/agent-presence` on the same
loopback listener with a dedicated bearer from `reader_token_file`. Provision
this separate 32–256 character URL-safe secret privately for both services;
restart both consumers when rotating it. Never put it in browser code, a URL,
logs or committed configuration. It is neither an MCP token nor OAuth access.
The endpoint requires a loopback peer and Host `localhost`, `127.0.0.1` or `[::1]`
(optionally with the actual listener port). It rejects `CF-Connecting-IP`,
`Origin`, duplicate Host headers and query parameters. Thus a tunneled request
cannot become an internal read, even with a valid reader token. Existing public
MCP/OAuth Host, Origin, authentication and rate controls remain unchanged.
Reader authentication failures use the existing bounded failure limiter.

The response includes all configured agents and no auth subjects or capabilities:

```json
{"version":1,"agents":[{"agent_id":"dot","label":"Dot","state":"online","last_seen_at":"2026-11-27T12:00:00Z"},{"agent_id":"gemini","label":"Gemini","state":"never","last_seen_at":null}]}
```

Responses use `Cache-Control: no-store`. The reader cannot write presence or use
its secret to call MCP. Disabled/unavailable presence returns `503`; the portal
must distinguish that from a successful `never` observation. No Cloudflare or
worker changes are required by this implementation.

## Security model

- **Authentication.** Every `/mcp` request needs `Authorization: Bearer hmcp_<id>_<secret>`. A missing or bad token gets `401` with `WWW-Authenticate: Bearer realm="homelab-mcp"` (plus `error="invalid_token"` when a token was sent).
- **Token storage.** Tokens carry 256 random bits. Only their SHA-256 is stored, in `/etc/homelab-forge/mcp/tokens.json`. That file is `root:homelab_mcp 0640`, written only by root through `mcp_tokens.py`.
  - The server refuses the file if it is group-writable or world-accessible. If the file is unreadable or invalid, the server rejects every request until it is fixed.
  - It reloads the file whenever it changes, so `create` and `revoke` take effect without a restart.
- **Expiry and scopes.** Every token expires (default 90 days, maximum 365) and carries a scope. Only `read` exists. The optional heartbeat additionally requires an administrator-configured authenticated identity mapping.
- **Rate limiting.** 10 failed attempts within 60 s from one client lock that client out for 5 minutes (`429` with `Retry-After`). A global ceiling of 120 failures per minute applies on top.
  - The client key is the `CF-Connecting-IP` header, used only when the TCP peer is loopback (cloudflared). It is used only to group failures, never as an identity.
- **Host and Origin checks.** `Host` must be in `allowed_hosts`, otherwise the response is `421`. A request that sends `Origin` must match `allowed_origins`, which is empty by default; otherwise the response is `403`. This blocks DNS rebinding and browser pages. Agents don't send `Origin`.
- **Database boundary.** This is the real security boundary:
  - The server connects as `homelab_mcp`. It has `SELECT` on a curated table list (the same list the Muse reader gets), no `CREATE`, owns nothing and belongs to no roles.
  - The role defaults to `default_transaction_read_only=on`.
  - Each SQL tool call runs in its own `READ ONLY` transaction with `statement_timeout` (5 s) and `lock_timeout`, and the transaction is always rolled back.
  - Rows are fetched only up to the cap. Long cells are truncated, and the response size is capped.
  - At most 4 queries run at once.
- **SQL guard.** The guard rejects multiple statements, writes, data-modifying CTEs, `SELECT INTO`, row locks, `EXPLAIN ANALYZE` and risky functions such as `pg_sleep`, `pg_read_file`, `dblink` and `set_config`. It is an early, readable rejection, not the boundary.
- **Process isolation.** The server binds loopback only (the config refuses anything else). It runs as the unprivileged `homelab_mcp` user with `ProtectSystem=strict`, an empty capability set, `IPAddressAllow=localhost`, a syscall filter and memory/task limits.
- **Logging.** Logs go to the journal: tool name, token id, status and duration. SQL text and tokens are never logged.

Known limits:
- Bearer tokens work with agents and CLIs that let you set a header: Claude Code, Cursor, Codex and scripts.
- Hosted connectors that only support OAuth (Gemini custom apps, claude.ai, ChatGPT) use the OAuth layer below with a pre-registered client; there is no dynamic client registration.
- Only one scope exists, so all tokens see the same tables.

## Install on Mission Control (root, not yet done)

Run these from a checkout of the reviewed commit. Steps 2 and 3 change the
live database, so take a backup first (`sudo deploy/mission-control/backup-postgres.sh`).

1. Create the service user and install the release:

   ```bash
   sudo useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin homelab_mcp
   release=/opt/homelab-forge/mcp/releases/$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)
   sudo install -d -m 0755 "$release"
   sudo install -m 0644 deploy/mission-control/mcp-postgres/{mcp_auth.py,mcp_oauth.py,mcp_presence.py,sql_guard.py} "$release"/
   sudo install -m 0755 deploy/mission-control/mcp-postgres/{mcp_server.py,mcp_tokens.py} "$release"/
   sudo ln -sfn "$release" /opt/homelab-forge/mcp/current
   sudo install -d -m 0750 -o root -g homelab_mcp /etc/homelab-forge/mcp
   sudo install -m 0640 -o root -g homelab_mcp deploy/mission-control/mcp-postgres/server.example.json /etc/homelab-forge/mcp/server.json
   ```

2. Create the database role. Peer authentication over the Unix socket maps OS user `homelab_mcp` to the role of the same name, so no password and no HBA change are needed:

   ```bash
   sudo -u postgres psql -X -v ON_ERROR_STOP=1 -d postgres -c "
     CREATE ROLE homelab_mcp LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION CONNECTION LIMIT 8;
     ALTER ROLE homelab_mcp SET default_transaction_read_only = on;
     ALTER ROLE homelab_mcp SET statement_timeout = '5s';"
   ```

3. Grant read access and verify it. The script ends by running `verify-mcp-readonly.sql`:

   ```bash
   sudo -u postgres psql -X -d forge_agent -f deploy/mission-control/mcp-postgres/sql/grant-mcp-readonly.sql
   ```

4. Create the first token. It is printed once, so copy it into the agent's secret store:

   ```bash
   sudo /opt/homelab-forge/mcp/current/mcp_tokens.py create --name "agent laptop" --ttl-days 90
   ```

5. Install and start the unit. Register it with the lifecycle script, using the inventory id `homelab-mcp`:

   ```bash
   mkdir -p /tmp/mcp-bundle && cp deploy/mission-control/mcp-postgres/homelab-mcp.service /tmp/mcp-bundle/
   sudo deploy/mission-control/services.py install homelab-mcp /tmp/mcp-bundle
   curl -s -H 'Host: 127.0.0.1:8791' http://127.0.0.1:8791/healthz          # {"status": "ok"}
   curl -s -o /dev/null -w '%{http_code}\n' -H 'Host: 127.0.0.1:8791' -X POST http://127.0.0.1:8791/mcp   # 401
   ```

## Cloudflare routing (no Access app)

Do this only after the local checks pass, so the hostname never points at a
missing or unauthenticated service.

1. **Tunnel route.** In tunnel `mission_control` (`49be3d89-712b-4cb7-87c2-2a2cdfee5cb2`, remotely managed), add a public hostname above the `http_status:404` catch-all:
   - `mcp.thedevex.com` → `http://127.0.0.1:8791`
   - Keep the existing `portal.thedevex.com` → `http://localhost:8090` route unchanged.
2. **DNS.** Add a proxied `CNAME` from `mcp` to `49be3d89-712b-4cb7-87c2-2a2cdfee5cb2.cfargotunnel.com`. The dashboard adds this automatically when the hostname is added to the tunnel.
3. **No Access application** for `mcp.thedevex.com`. Authentication is the bearer token.
4. **Optional:** add a WAF rate-limiting rule on `mcp.thedevex.com/mcp`. Also make sure Bot Fight Mode or a challenge doesn't block non-browser clients.

Check it from outside:

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://mcp.thedevex.com/mcp          # 401
curl -s https://mcp.thedevex.com/mcp -H "Authorization: Bearer $HOMELAB_MCP_TOKEN" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

## Client configuration

Keep the token in an environment variable such as `HOMELAB_MCP_TOKEN`, not in a committed file.

Cursor (`~/.cursor/mcp.json` or `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "homelab-postgres": {
      "url": "https://mcp.thedevex.com/mcp",
      "headers": { "Authorization": "Bearer ${env:HOMELAB_MCP_TOKEN}" }
    }
  }
}
```

Claude Code:

```bash
claude mcp add --transport http homelab-postgres https://mcp.thedevex.com/mcp \
  --header "Authorization: Bearer $HOMELAB_MCP_TOKEN"
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.homelab-postgres]
url = "https://mcp.thedevex.com/mcp"
bearer_token_env_var = "HOMELAB_MCP_TOKEN"
```

Clients that only speak stdio, such as Claude Desktop, can use the `mcp-remote` bridge:

```json
{
  "mcpServers": {
    "homelab-postgres": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://mcp.thedevex.com/mcp",
               "--header", "Authorization:Bearer ${HOMELAB_MCP_TOKEN}"],
      "env": { "HOMELAB_MCP_TOKEN": "hmcp_..." }
    }
  }
}
```

## Token operations

```bash
sudo /opt/homelab-forge/mcp/current/mcp_tokens.py list
sudo /opt/homelab-forge/mcp/current/mcp_tokens.py create --name "ci agent" --ttl-days 30
sudo /opt/homelab-forge/mcp/current/mcp_tokens.py revoke <id>
```

To rotate a token, create the new one, update the client, then revoke the old one.
Revoked and expired tokens stay in the file for audit.

## Tests

```bash
cd deploy/mission-control/mcp-postgres && python3 -B -m unittest -v
```

The integration class starts a disposable PostgreSQL 16 cluster under `/tmp`.
It applies the Forge schema, creates `homelab_mcp`, runs the grant script and
checks the following:
- Writes, DDL and temporary tables fail at the database level.
- Ungranted tables are denied.
- The statement timeout fires.
- The verify script catches an added write grant.

These tests need `/usr/lib/postgresql/16/bin` and a non-root user. The suite is
registered in `backend-suites.py`.

## OAuth 2.1 (Gemini custom apps and other hosted connectors)

Enable with `"oauth": {"enabled": true}` in `server.json` (issuer defaults to
`https://mcp.thedevex.com`). Static `hmcp_` tokens keep working unchanged; `/mcp`
accepts either a static token or an OAuth access token (`hmcpat_...`), and both
map to the same read-only scope and database role.

| Endpoint | Purpose |
|---|---|
| `GET /.well-known/oauth-protected-resource` (and `/mcp` suffix) | RFC 9728 metadata; also advertised in the 401 `WWW-Authenticate` `resource_metadata` parameter. |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 metadata. No `registration_endpoint`: clients are pre-registered. |
| `GET/POST /oauth/authorize` | Validates client, redirect URI (Google `oauth-redirect{,-sandbox,-test}.googleusercontent.com` `/r/` and `/a/` prefixes only), `response_type=code`, PKCE `S256` (required) and `scope=read`; shows a read-only consent page; the operator signs in with the single operator password (scrypt hash, CSRF cookie + HMAC field, lockout after 5 failures). Redirects with a single-use code valid 60 s. |
| `POST /oauth/token` | `authorization_code` (code, redirect_uri, PKCE verifier, client secret via `client_secret_post` or `client_secret_basic`) and `refresh_token`. Access tokens last 1 h, refresh tokens 90 days and rotate on every use; replaying a used refresh token or code revokes the whole grant. |
| `POST /oauth/revoke` | RFC 7009. Revoking a refresh token revokes its grant; always 200 after client auth. |

State: client records (secret SHA-256 only) in `/etc/homelab-forge/mcp/oauth-clients.json`,
the operator scrypt hash in `/etc/homelab-forge/mcp/operator.json` (both
root:homelab_mcp 0640), and hashed codes/grants/tokens in
`/var/lib/homelab-forge/mcp/oauth.sqlite3` (systemd `StateDirectory`, 0700).

```sh
sudo ./mcp_oauth.py operator set-password --generate /root/homelab-mcp-operator-password.txt
sudo ./mcp_oauth.py client create --name gemini --secret-file /root/homelab-mcp-gemini-oauth-client.txt
sudo ./mcp_oauth.py client list
sudo ./mcp_oauth.py grant list
sudo ./mcp_oauth.py grant revoke <grant_id>          # or: grant revoke-client <client_id>
sudo ./mcp_oauth.py client revoke <client_id>
```

The CLI prints only ids; secrets go to new 0600 files and are never echoed.

Gemini: Settings, Connected Apps, Custom apps, Add a custom app; URL
`https://mcp.thedevex.com/mcp`; under Advanced features, Show more, paste the
client id and secret from the client file; approve the consent page with the
operator password.
