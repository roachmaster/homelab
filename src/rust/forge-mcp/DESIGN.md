# Mission Control MCP API — design

**Status: P1 implemented in source (2026-11-26); not deployed.** The
contract crate and the `forge-mcp-host` binary exist and are checked:
the server speaks MCP over stdio and Streamable HTTP, with an
in-memory fake backend (default, zero effects) and a live backend
(read-only Postgres reads; dispatch HTTP writes). The live backend has
NOT been exercised against the production database or runner, the
`decide` dispatch body shape is unverified against the live runner,
and resource subscriptions (P3) are not implemented. Labels follow the
repository convention: *implemented* means source exists and is
checked; *proposed* means this document is the authority until code
replaces it.

## Why

Every agent that works with Mission Control today integrates by hand:
the runner's dispatch HTTP API with its custom headers, direct
read-only Postgres over an SSH tunnel, `gh` for branches and PRs, and
SSH + `psql` for everything else. Each integration re-learns the same
lessons (idempotency keys, base-revision pinning, lease discipline) and
repeats the same failure modes — most recently, submissions rejected
with one opaque `result_mismatch` code for three different causes.

MCP (Model Context Protocol) is the standard surface agent clients
already speak. One typed MCP server for Mission Control gives every
present and future agent — cloud or local — the same discoverable
tools, resources, and prompts, with the Forge rules enforced at the
server instead of re-taught per client.

## Capability

A single MCP server process on Mission Control that:

- exposes Forge work operations as **tools** (submit, get, cancel,
  decide, resume),
- exposes workflow state as **resources** (`forge://workflows/{id}`,
  `forge://tasks/{id}`, `forge://events/recent`) with subscriptions
  fed by the existing `forge_coding_jobs` LISTEN/NOTIFY channel,
- exposes the Forge task-authoring format as a **prompt**
  (assignment and acceptance criteria as two separate blocks),
- and does all of it **without becoming a new write path**: writes are
  translated 1:1 into runner dispatch calls; reads use a read-only
  database role in the `muse_events` pattern. The server holds no
  authority of its own — it presents the caller's profile and grants,
  exactly as dispatch sees them.

### Non-goals

- No raw SQL tool. Reads are curated queries behind typed responses.
- No bypass of dispatch: no direct writes to `forge_agent` tables, no
  lease/claim surface (claim discipline stays inside the transports
  that already own it).
- Not a portal replacement; the portal remains the human UI. The MCP
  server and the portal read the same projections.
- No multi-tenant hosting. One server, one Mission Control, LAN/trusted
  callers only.

## Routes (protocol surface)

Transports, in priority order:

1. **Streamable HTTP** (the current MCP transport) on a loopback or
   tailnet bind — the normal path for remote agents.
2. **stdio** for local tooling and conformance testing (MCP Inspector).

Tools (v1 = read-only; v2 adds writes). Descriptors are data in
`src/lib.rs` (`TOOLS`), annotated with MCP `readOnlyHint` /
`destructiveHint` so clients can gate them honestly:

| Tool | Kind | Backed by |
| --- | --- | --- |
| `forge.work_get` | read | dispatch `work_get` / read role |
| `forge.workflow_get` | read | read role |
| `forge.events_list` | read | read role (`coding_events`) |
| `forge.work_submit` | write | dispatch `work_submit` |
| `forge.work_cancel` | write | dispatch `work_cancel` |
| `forge.decide` | write | dispatch `plan.decide` / `review.decide` |
| `forge.work_resume` | write | dispatch `work_resume` |

Later candidates (own design slices, not v1/v2): Jenkins build status,
delivery/portal status, branch and PR state, the resubmission queue as
resources. Each is a new request variant plus a registry entry — the
payload sum stays finite and application-owned.

## Required and provided dependencies

Consumer-owned port: **`MissionControlPort`** (`src/lib.rs`), shaped
like `forge_core::IoClient` — `execute(request) -> Result<response,
error>` with owned `McRequest` / `McResponse` / `McError` sums.

Implementations (separate crates or feature-gated modules, never in
this contract crate):

- `DispatchClient` — HTTP to the runner dispatch API
  (`127.0.0.1:18083`); owns header/envelope encoding and the bearer
  credential; the only write path.
- `ReadClient` — read-only Postgres role; owns the curated queries
  behind the read tools and resource reads.

Errors are part of the contract: `McError` keeps dispatch rejections
**distinguishable** (stale base, stale digest, revoked grant). This is
a hard requirement, imported from the dispatch-digest-diagnosis defect
in the resubmission queue — the MCP layer must not re-collapse them
into one opaque code.

## Assembly order (Wire Forge Providers)

1. Bootstrap reads typed config: bind address, transport selection,
   dispatch endpoint, read-role DSN source, credential paths.
2. A registrar stages the factories for the selected port
   implementations and the protocol layer; registration validates
   config and identity metadata only — no sockets, no connections.
3. The startup plan validates and **freezes**; only then are clients
   constructed (dependency order: credential source → dispatch/read
   clients → protocol layer → transport).
4. Readiness is established before the transport opens admission.
5. Shutdown stops admission first, quiesces in-flight calls, then
   releases clients; a factory that fails mid-construction cleans up
   its partial acquisitions.

Scopes: the server process is the singleton owner of its port
instances; each MCP session is request-scoped state only — no session
may hold a database transaction or a dispatch call across requests.

## Package boundaries

- `forge-mcp` (this crate): contract + registry data + design.
  Contract-only, per the module rules — no I/O, no provisioning, no
  registrar of its own.
- Implementation crates (proposed names): `forge-mcp-dispatch`,
  `forge-mcp-read`, and the host binary `forge-mcp-host` that owns
  composition, config, and the systemd-facing lifecycle. The protocol
  layer (tool/resource/prompt routing over the port) lives with the
  host or its own crate — decided at implementation time by the
  substitution proof below, not by this design.
- The MCP Rust SDK (`rmcp`) is a dependency of the host/protocol
  layer only, behind the port, so the SDK is substitutable without
  touching the contract — the same reason hand-rolled JSON-RPC stays
  a legal fallback if the SDK's churn becomes a problem.

## Auth and trust

- The server authenticates callers with the same bearer-credential
  pattern as the runner token (systemd credential file, never in
  config or chat).
- Authorization is **delegated**: the server forwards the caller's
  agent profile to dispatch, and dispatch grants decide. The server
  never widens a caller's authority — a lesson priced in by the
  2026-11-25 grant rotation, after which undiagnosable rejections
  cost two filings.
- Write tools are annotated destructive where they are (cancel,
  decide) so MCP clients surface their own approvals; the server adds
  no second approval layer of its own.

## Deployment (proposed)

A systemd unit on Mission Control beside `forge-agent-runner`,
released through the same snapshot mechanism as the other services —
source presence never implies deployment. Config selects transports;
stdio needs no unit.

## Phasing

- **P0** — this design + contract skeleton. *(done)*
- **P1** — read-only tools and resources over `ReadClient`; stdio
  transport; conformance pass with MCP Inspector. *(implemented in
  source; verified over stdio and Streamable HTTP against the fake
  backend; MCP Inspector pass and live-DB verification outstanding)*
- **P2** — write tools over `DispatchClient`; Streamable HTTP;
  per-cause error mapping proven against a staging database.
  *(implemented in source; live verification outstanding — the
  dispatch contract still returns an undifferentiated
  `result_mismatch`, which the client reports with all three causes
  named rather than guessing)*
- **P3** — resource subscriptions over LISTEN/NOTIFY.
- **P4** — qualification: contract tests (descriptor ↔ behavior drift
  checks), substitution proof (a second `MissionControlPort`
  implementation — in-memory fake — drives the same protocol tests),
  then a Forge task files the rollout like any other capability.

## Checks and gaps

Proposed checks (none executed yet — no runtime exists):

- Descriptor/behavior drift test: every `TOOLS` entry maps to exactly
  one `McRequest` variant and one handler.
- Rejection-before-effect: a stale-base submit fails at the port
  boundary with `McError::StaleBase` and produces no dispatch call.
- Substitution: the in-memory fake port passes the protocol test
  suite unchanged.
- Freeze discipline: with an invalid config, startup fails before
  any socket bind or database connection.

Known gaps this design does not close: the underlying dispatch
rejection codes themselves (owned by the dispatch-digest-diagnosis
defect — this design consumes its outcome), workflow revival
(#24/#27), and the manifest-return loop. The MCP server surfaces
those systems; it does not fix them.
