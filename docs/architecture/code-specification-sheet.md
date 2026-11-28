# Code Specification Sheet — Forge workspace

**State:** `feature/muse` @ `2c58fb6` (identical tip on `feature/gemini`), 2026-11-27.
**Tests:** workspace 1,424 passing / 0 failing. Guards: `check_dependencies.py`,
`check_core_layers.py`, `check_process_exec.py` all pass (see §4).
**How to read this sheet:** §1 specifies what each crate is contracted to do.
§2 is the behavioral specification: every BDD feature in
`src/rust/forge-scene/features/`, with the scenarios that pin the behavior.
A behavior is specified only when a scenario names it; the bindings in
`src/rust/forge-scene/tests/bdd_*.rs` drive the real crates, never copies.

---

## 1. Workspace specification

### Framework (forge-core and its direct extensions)

| Crate | Contract |
|---|---|
| forge-core | Framework kernel: `IoClient` (one typed `execute` boundary per I/O family, consumer-owned request/response/error), `Factory` / `InstanceProvider` (Singleton, Transient), the bootstrap registry (stage → validate → freeze → create), bridge/ingress routing with typed headers, command descriptors. No I/O of its own. |
| forge-foundation | Provider lifecycles and typed per-environment wiring on top of forge-core: providers, slots, decorators, env builder. |
| forge-scene | Build-time scene codegen (schema + template + data) and the typed BDD layer: feature grammar, step registry, voice rules, renderer, JSON Schema checker. |
| forge-template | Bounded, typed, schema-checked Mustache-like rendering. Renderers are registered at startup and rendered on demand. |
| forge-jobs | Durable jobs that plug into any host: front desk and worker share one store; the store opens on first job; tests swap only the store. |

### Work capability (the deploy/ Python migration, in Rust)

| Crate | Contract |
|---|---|
| forge-work | Typed work capability: frozen catalog, canonical `coding_events` vocabulary, Bridge-fronted WorkService, the three driven WorkStore ports (read, admission, decision), and the pure v2 domain contracts (peer review, feedback, repair/retry policy, recovery, handoff). No SQL, no I/O. |
| forge-work-worker | Native execution: the CodingWorker loop, provider CLI execution, workspace leases, WorkerDB role ports. Bounded subprocesses run through the `process_exec` IoClient (typed argv/env/stdin, captured output, time bound); streaming and process-group launchers are allowlisted in `check_process_exec.py`. Performs no SQL. |
| forge-work-publication | Publication and delivery: CI gate contracts, verification, candidate publication clients, merge and post-merge, hosted checks, external delivery, Jenkins lifecycle, native CI. Its bounded subprocess adapters (Jenkins observer, verifier AST) run over the worker's `process_exec` IoClient; its registrar requires that binding at freeze. |
| forge-postgres | Dedicated Postgres adapter: synchronous facade over tokio-postgres speaking the `forge_coding_worker_api` and `forge_coding_reconciler_api` SQL surfaces directly. Rust stores never route through the retired forge-pg-proxy service. |
| forge-jenkins | Jenkins bridge: candidate build triggering, phase observation, lifecycle events, cleanup-stage confirmation as a hard gate for review and publication. |
| forge-usage | Provider usage collectors (per-agent token/usage accounting) with no forge-* dependencies beyond forge-core. |
| forge-host | Production composition roots: forge-coding-worker, forge-publication-worker, forge-runner binaries wiring the crates to PostgreSQL and the host. |
| forge-agent-driver | Provider CLI driver processes (per-attempt agent execution) as a standalone composition crate. |
| forge-services | Host service control (systemd units for the Forge services) as a standalone crate. |
| forge-portal-release-actor | Portal release actions (promote/retire release artifacts) as a standalone crate. |
| forge-studio | Forge Studio: private game-project build control, signed preview listener and build worker over the `forge_game` schema. |

### MCP surfaces

| Crate | Contract |
|---|---|
| forge-mcp | Mission Control MCP API contract: typed, discoverable surface (6 tools) over the Forge runner dispatch API and read-only workflow state; request parsing and response shaping. |
| forge-mcp-host | Composition root for forge-mcp: typed config selects the backend (fake by default — zero external effects; live only when configured), then serves MCP over stdio or Streamable HTTP. |
| forge-mcp-db | The live read-only database MCP (mcp.thedevex.com): PostgreSQL tools over Streamable HTTP, static bearer tokens, a built-in OAuth 2.1 authorization server, connection presence. |

### Pre-existing application crates

| Crate | Contract |
|---|---|
| forge-agent | Agent assembly: per-project managed writers created after registry freeze; host/dispatch over IoClient ports. |
| forge-ide | The Forge IDE core: filesystem, remote sources, AST graph, tasks — all behind IoClient boundaries with in-memory doubles in tests. |
| forge-ide-ui | WASM UI model for the portal (serde-only), including the sanitizing markdown renderer for Forge Chat. |
| forge-ide-web | Generates the portal's HTML/CSS/JS assets from Rust; no handwritten app JS. |
| forge-ide-web-host | Portal host: relays chat to the local model, owns the selected asset source and relay instances. |
| forge-local | Local node application: disk commands and the local bridge over typed descriptors. |
| forge-system | System/disk capability primitives used by the local node. |
| forge-terminal | Terminal host for durable jobs executables. |
| forge-remote / forge-remote-https | Remote source access for the IDE, plain and over HTTPS. |
| forge-game-runtime | Game runtime: tick, entities, snapshot/trace for the Forge engine. |

---

## 2. BDD specification index

12 product features, 73 scenarios, plus the framework's worked example
(`dog.feature`, 9 scenarios). All feature files pass the forge-scene voice
rules (0 errors).

### Framework behavior

**Feature: Providers hand out values and the registry checks startup**
(`forge_core_providers.feature` → forge-core)
- A singleton shares its value
- A transient builds for every lease
- A lazy provider waits for its first lease
- The first lease builds the lazy value once
- Startup refuses a missing binding
- Startup refuses a group staged too early
- A ready registry hands back its factories

**Feature: Durable jobs plug into any host** (`forge_jobs.feature` → forge-jobs)
- The front desk and the worker share one store
- The store stays closed until the first job
- The first job opens the store
- Each request for a worker starts a new one
- A worker with no store
- Tests swap only the store

**Feature: Templates are registered at startup and rendered on demand**
(`forge_template_renderers.feature` → forge-template)
- One renderer serves every page
- A broken template is found when it is first used
- Builtin methods are there unless the host adds its own
- Two templates cannot share a name

**Feature: Disk registration and local host bridge enforce typed boundaries**
(`forge_system_local.feature` → forge-system, forge-local)
- A relative disk directory is rejected during registration
- A missing absolute directory fails at creation rather than registration
- Bootstrapping without a disk directory omits disk commands
- Bootstrapping with a valid directory publishes all disk commands
- An admitted disk.list returns entries in deterministic order
- A header and payload mismatch is rejected by the local bridge

### Work admission and workflow behavior (forge-work)

**Feature: Work bridge enforces admission and typed header agreement**
(`forge_work_bridge.feature`)
- Open admission completes a valid list request
- Closed admission rejects work without touching ports
- A header and payload mismatch is rejected before effects
- An unknown destination is rejected before effects
- An unsupported contract version is rejected before effects
- A port error propagates to the caller

**Feature: Model route recovery follows the evidence**
(`model_route_recovery.feature`)
- A stopped binary pin is recovered on verified evidence
- A replayed pin recovery returns the original receipt
- A quota-blocked task routes to its backup model
- A second route is refused
- A quota wait is recorded when no reset is reported
- The task resumes after the reported reset
- Resumes stop at the episode limit

### Worker execution behavior (forge-work-worker)

**Feature: Worker process execution honors typed bounds and registration**
(`process_exec.feature`)
- An empty command is refused before spawning
- Standard input is echoed back cleanly
- A cleared environment strips ambient variables
- A slow command is killed at its deadline
- Registration stages once and rejects duplicates
- Git runner masks failures behind a hardened request

**Feature: ACP permission follows the captured call**
(`grok_acp_permissions.feature`)
- A complete descriptor proves the captured command
- A descriptor naming another command proves nothing
- Changed arguments at permission time are rejected
- Progress cannot rewrite the captured arguments
- A call with malformed native metadata is refused at capture
- A pathless grep reads the workspace
- A grep beyond its bounds is refused

**Feature: Worker cooldown withholds exhausted profiles safely**
(`worker_cooldown.feature`)
- Fresh state admits all configured profiles
- A quota diagnostic withholds only the exhausted profile
- Admission resumes once the retry window expires
- Replaying the same attempt diagnostic is idempotent
- A malformed diagnostic is ignored safely
- A missing state root fails closed

**Feature: Worker supervisor verifies systemd scope stop proofs strictly**
(`worker_supervisor.feature`)
- An invalid scope name is rejected without running commands
- An inactive unit with MainPID zero and an unpopulated cgroup proves stopped
- A populated cgroup prevents a stop proof
- An active unit with a live MainPID prevents a stop proof
- A mismatched ControlGroup prevents a stop proof
- Stopping a unit issues systemctl stop and verifies with show

### Publication behavior (forge-work-publication)

**Feature: Publication registrar and process-backed ports honor contracts**
(`forge_work_publication.feature`)
- Freezing publication without process execution fails
- Staging after process execution freezes and refuses duplicates
- Observer execution returns stdout on zero exit
- Observer execution translates a time-bound failure
- Observer execution rejects an empty argument list
- Verifier AST parsing decodes JSON and surfaces stderr on failure

### MCP host behavior (forge-mcp-host)

**Feature: MCP host advertises surfaces, dispatches requests, and publishes schemas**
(`forge_mcp_host.feature`)
- Server info names the service and advertises all surfaces
- Work get returns the fake task view
- Workflow get returns an enabled implement workflow
- Events list caps at five newest first and honors smaller limits
- Control mutations return a fresh UUID on submit and echo task IDs
- Input schemas name required arguments and stay open for unknown tools

---

## 3. Verification gates

- **Workspace tests:** `cargo test --workspace` — 1,424 passed / 0 failed at
  this sheet's commit. Branch pipeline order (standing spec): fmt, imports /
  pub-use check, compile, unit + component tests, in-memory integration
  tests; out-of-memory tests (browser, DB, NATS) run on the main pipeline only.
- **Dependency guard:** `check_dependencies.py` — crate edges and external
  dependencies match the recorded architecture; new edges must be registered.
- **Layer guard:** `check_core_layers.py` — forge-core module layering
  (provider/io_client/command/ingress/bootstrap independence). 0 errors.
- **Process-exec guard:** `check_process_exec.py` — bounded subprocesses go
  through the `process_exec` IoClient; 18 streaming/process-group sites
  across 16 files are explicitly allowlisted with pinned counts. 0 errors.
- **BDD voice rules:** every feature file passes forge-scene's voice checks
  (Given phrases state capability, Then phrases state outcomes, no routing
  words in outcomes).

## 4. Coverage

llvm-cov over the full workspace test run (1,424 passed / 0 failed),
own code only (registry dependencies excluded), measured on the
merged tree at `33bac6c7`:

**Workspace: 55.5% lines (92,732 / 166,956)** — up from the 53.5%
baseline (86,248 / 161,084) taken before the mantra/BDD pass.

| Crate | Lines | vs baseline |
|---|---|---|
| forge-work | 37.6% | 32.2% |
| forge-work-worker | 42.6% | 39.3% |
| forge-work-publication | 34.3% | 33.8% |
| forge-mcp-host | 32.3% | 0.0% |
| forge-jenkins | 74.3% | — |
| forge-usage | 80.7% | — |
| forge-portal-release-actor | 38.1% | — |
| forge-agent-driver | 44.2% | — |
| forge-services | 24.0% | — |
| forge-studio | 21.7% | — |
| forge-host | 22.1% | 22.1% |
| forge-mcp-db | 21.4% | 21.4% |
| forge-postgres | 7.9% | 7.9% (live-DB-gated) |

Pre-existing crates remain 68.8–97.7%. The low three are structural:
forge-postgres is exercised against a live database, forge-host is
composition wiring, and forge-mcp-db's OAuth surface awaits its own
test pass.
