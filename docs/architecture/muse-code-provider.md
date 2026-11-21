# Muse Code provider

Status: source implementation for schema19 / `forge-coding-v10`. Muse email
remains the primary integration. Native Muse execution defaults **disabled** in
both admission and worker configuration; metadata-only selection never enables
work claims. No live schema or service activation is part of this integration.
Deployment,
authentication, included account allowance and the actual nested shell sandbox
must be qualified separately. A verified model catalog is not evidence of an
available subscription allowance or a completed model task.

Muse is selected through the existing `WorkProfile` → immutable `PinnedRecipe`
→ `CodingWorkerFactory` → `CommandRunnerProvider` path. It uses the same scoped
workspace, PostgreSQL lease, cancellation, reconciliation, result publication,
independent review and CI gates as other providers. No task may select a binary,
auth path or permission override. Default autonomous roles remain Antigravity
author and Codex reviewer; adding a profile does not authorize overriding that
project workflow. A future configured role can select Muse without changing the
controller. Existing workflows retain their frozen recipes and policy digests.

## Verified native contract

The pinned native executable is Muse `1.4.24-R9131.1`, SHA-256
`d752c45dce079ee29e118804b091d81ac2b1b89b67f317b4a162fa1af1206465`.
The launcher is deliberately bypassed. Installed help and an offline echo
fixture establish the fixed argv:

```text
muse-bin exec --json --provider meta --model <configured-model>
  --workspace /run/forge-coding-workspace
  --prompt-file /run/forge-coding-attempt/prompt.txt
  --approval-mode never --approval-judge off
  --disable-reminders --disable-request-user-input
  --no-foreign-personal-context --trust-workspace
  --contributor-mode off -c agents.execution_capacity=1
```

Plan and Review also select `--disable-write`; the outer read-only mount protects
against shell writes as well. Muse's nested sandbox stays enabled. The adapter
refuses a supervisor without the existing enforcing capability. There is no
`--yolo`, paid API fallback, observer agent, approval-judge model call, automatic
login, account rotation, resumed session or arbitrary CLI argument field.
`agents.execution_capacity=1` was accepted by an offline echo probe, without
Meta model requests. A meaningful no-model shell test inside the real supervisor
is a separate activation gate; an echo reply alone does not prove shell access.

Each attempt starts a fresh Muse session and fresh HOME/XDG configuration under
its scratch lease. Only the selected native `auth.json` is copied using no-follow
directory handles and an owner/private regular-file check, then mounted read-only.
Credentials are never parsed, logged or returned. User settings, MCP servers,
hooks, plugins and other providers' state are not imported. Explicit repository
trust loads the task workspace's project instructions and skills. The parent
supervisor still hides database sockets, service configuration and credentials,
drops capabilities, owns the cgroup and enforces the1800-second deadline and
TERM→KILL cleanup before accepting terminal output. A shared native-credential
lease retains directory and inode handles until both launcher and cgroup stop
are proved. Only that owned inode is removed; parent replacement cannot redirect
cleanup into the host account. Uncertain stop or inode replacement produces
`needs_reconcile` with a nonsecret parent-owned recovery witness. Setup and
partial-copy failures clean up before returning ownership.

## Structured output and accounting

The incremental parser accepts MSP JSONL schema1, preserves stream sequence
ordering and binds one session, one command/run and one terminal result. Output
is accepted only from the session stream and a linked, started run. A coherent,
nonempty `run.terminal.completed` result and verified stopped process scope are
both required; exit0 alone is insufficient. Malformed JSON, changed identities,
repeated terminal/usage records and failed/cancelled runs never become success.
Native cancellation remains a cancelled outcome. No tool arguments or private
reasoning are persisted by this parser.

`run.usage.models` contributes reported input/output/cached counters once for
the bound run; child-inclusive or malformed aggregates are rejected. Missing
counters stay unknown. Provenance is `muse-run-usage-v1` / `fresh_session`.
This adapter pins **Meta**, whose [prompt-cache contract](https://dev.meta.ai/docs/prompt-caching)
defines cached tokens as an input subset: total=input+output; cache is displayed
separately and never added again. This convention must not be applied to another
Muse provider without its own verified contract. Coverage reports whether exact
counters were supplied; it never estimates tokens from text.

The authenticated manual `usage --json` observation returned schemaVersion1,
`allowance:null` and `reason:no_allowance`. That means **Not reported**; it does
not mean zero usage, unlimited use, a paid allowance or no subscription. Refresh is manual-only, including when no execution profiles are enabled. It
runs the exact pinned `usage --json` metadata command with a private HOME and
an explicit auth lease, then proves the owned process group has stopped before
credential cleanup. Uncertain cleanup retains the private temporary directory
for explicit reconciliation and reports unavailable. Refresh
uses the selected metadata provider, never consumer OAuth endpoint polling or
background allowance requests. Account allowance and Forge-attributed job token
counts remain separate scopes.

## Execution sequence

```mermaid
sequenceDiagram
    participant Portal
    participant API as WorkService
    participant DB as PostgreSQL
    participant Factory as CodingWorkerFactory
    participant Runner as CommandRunnerProvider
    participant Scope as SystemdSupervisor
    participant Muse as Pinned Muse CLI
    Portal->>API: admitted WorkSubmit(profile, mode)
    API->>DB: immutable recipe + workflow authority
    Factory->>DB: claim matching grant / P10 digest / lease
    Factory->>Runner: owned workspace + exact lease
    Runner->>Runner: hash binary; fresh HOME; private auth stage
    Runner->>Scope: fixed argv + immutable prompt file
    Scope->>Muse: unprivileged isolated process / nested sandbox
    Muse-->>Runner: bound MSP output + run usage + terminal
    Runner->>Scope: stop; verify cgroup empty
    Runner->>Runner: cleanup owned auth inode after proven stop
    Runner->>DB: chunked result + provenance + stop receipt
    DB-->>API: normal independent review / repair / CI events
    API-->>Portal: persisted status and task accounting
```

## Upgrade and evidence

Additive migration0019, after the unchanged Grok migration0018, extends the exact backend, policy and usage-provenance allowlists;
it changes no historical task, attempt, Plan, result, reconciliation or workflow
row. Existing P1–P9 policy IDs remain valid database history. Worker readiness and
claims still require the exact recipe policy digest; P10 cannot claim a P9 task.
Database roles, header routing and the source-bound three-stage workflow remain
unchanged. Strict catalog/verifier expectations include M19. Grok P9 keeps digest
`8191ffeda267b598c9d550a81e3f272d706c00e37731c7612c8ca3cb7d4ecf0e`;
combined P10 uses
`58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523`.
Migrations0001–0018 remain byte-identical.

Focused tests use the captured offline echo fixture, fake owned child processes
and actual disposable PostgreSQL16. They cover malformed output, stream/run
binding, failed/cancelled/empty terminal results, account-state isolation,
selected factory paths, token provenance, schema18→19 old receipt preservation, disabled-profile receipt replay and policy
claim isolation. They make no Meta model requests and do not mutate production.
