# Forge IDE web host

A **loopback-bound, single-operator prototype** serving the generated IDE UI
and adapting one browser command gateway to the existing authenticated remote
IDE inspection task. Explicit HTTPS-origin configuration supports an access-controlled
TLS proxy; this is not an unrestricted public service, editor or multi-user login.

## Composition and public API

`register_host(id, HostConfig)` validates typed configuration and stages a
`HostFactory` without reading files, opening sockets or constructing clients.
The application registers, validates/freezes, then invokes `Factory<Host>::create`.
Creation verifies the generated asset manifest, creates registered HTTPS clients,
wraps them in explicit `Singleton` providers, injects them into the existing
`RemoteInspectTask`, installs the shared-bridge routes and binds a loopback listener.
The host never imports the generator or changes generated assets.

`HostFactory::create_with_ports` supports explicit consumer-owned `InspectionPort`
substitution. Its map must exactly match configured nodes. That is a prepared-task
port, not a service locator or arbitrary renderer/client substitution claim.
`tests/consumer.rs` is an external crate consumer of the **public imports**, not an
included private example. Native code is isolated from portable `forge-ide-ui`.

All construction, use and destruction occur on the same synchronous owner outside
an async runtime. The HTTPS adapter owns its bounded runtime; this host does not
wrap it in another runtime. Connections are serviced sequentially, with no spawned
request threads or unbounded application queue. The OS listen backlog is not a
scheduler. Stop is checked during request reads and immediately before dispatch;
already admitted synchronous remote work may finish or reach its existing timeout.

## Run

Build generated assets using the repository's UI asset target first. With existing
configured DER credentials (the command does **not** provision or trust identities):

```sh
cargo run --manifest-path src/rust/Cargo.toml -p forge-ide-web-host -- \
  serve --config /absolute/path/ui-host.json
```

By default, open the printed `READY http://127.0.0.1:PORT` origin and enter the printed
one-use `PAIRING` code in the UI. Do not put this secret in a URL or publish a
terminal capture. Refresh resumes the existing memory-only session via its cookie.
Restart invalidates the session. `Ctrl-C`/termination requests orderly shutdown.

For a supervised service using the default pairing mode, append `--pairing-file /run/forge-web/pairing-code`.
Its parent must be an existing owner-only directory and the absolute destination
must not exist. The code is published complete with mode `0600`, without replacing
files or following destination symlinks; it is then omitted from stdout. A failed
publication aborts startup before `READY`. Use a private systemd runtime directory
that is removed on stop. Never place this file beneath served assets or a project.

Config JSON is only an external codec. Paths are config-relative:

```json
{
  "bind": "127.0.0.1:0",
  "assets": "generated-ui",
  "credentials": {
    "chain": ["client.der"], "key": "client.key.der", "roots": ["ca.der"]
  },
  "nodes": [{
    "key": "lab", "label": "My Forge node", "projects": ["workspace"],
    "address": "127.0.0.1:7443", "server_name": "localhost",
    "node": "node-1", "incarnation": "configured-incarnation"
  }],
  "timeout_ms": 5000,
  "response_bytes": 8388608
}
```

The host allowlist is an additional restriction, **not** delegated user identity.
The agent independently authenticates this host's mTLS service certificate and
checks its operation/project grants. Effective access is their intersection.
No browser-controlled URL, root path, arbitrary header identity or redirect relay
exists. Configuration is trusted startup input, not a browser API.

Set `"alternate_cooked_assets": "pinned-cook-directory"` to supply copies of
cooked game files missing from `assets/game-assets`. Point to the resolved
cooked generation directory containing `levels/` and `atlas/`. The host uses
the primary cooked manifest and verifies alternate bytes against its digests.
An unavailable alternate leaves primary assets usable.

Grant the host read access to the cooker's archive directory and set
`cooked_replay_archive` to that path.
Build releases with `FORGE_COOKED_REPLAY_ARCHIVE` set to that directory to retain
each manifest and its levels by manifest SHA-256. Replay import can then
retrieve the exact level bytes across releases. The active cook is served by
its digest even when an archive directory is unset.

The host registrar stages typed recipes for the asset bundle and archive,
Chat relay, Studio relay, and independent optional Work and Usage relays. The host factory opens the configured directories
and reads relay credentials after registry freeze. Each assembled host owns
one of each configured resource and shares the ready instances with its handlers.

### Studio

Configure the optional `studio` block with the private Studio API `endpoint`,
its `token_file`, and the separate HTTPS `preview_origin`. The web host checks
the browser session, CSRF token, and configured project grants before dispatch.
Open **Forge Studio** from the Game Engine design page or go to `/studio`.
[Studio service setup](../../../deploy/mission-control/studio/README.md)

### Coding assignments and agent usage

The optional `work` block enables durable coding assignments through the existing
private runner. It is disabled when absent and uses its own transport and grants;
Chat, Mail, GPU availability and the node's read-only inspection grants do not enable it.
The runner must independently configure its matching Work service and schema.

```json
"work": {
  "endpoint": "127.0.0.1:18081",
  "token_file": "/etc/homelab-forge/web/runner.token",
  "allowed_projects": ["homelab"],
  "allowed_profiles": ["agy-plan-v1"],
  "allowed_modes": ["plan"]
}
```

`forge_ide_web_host::{WorkConfig, WorkMode}` supplies the typed public configuration.
`register_host` stages the Work factory without reading its token. After startup
validation and registry freeze, `HostFactory` creates one ready relay, wraps it in
`Singleton`, and injects it into `Host`. Requests acquire that host-owned instance;
neither requests nor workers consult a registry. The native bearer credential is
read only at creation and never forwarded to the browser.

Use `POST /forge/v1/dispatch` with the normal session/CSRF admission and existing
four Forge headers. The admitted destinations are `ide.work.catalog`, `.submit`, `.get`, `.list`,
`.events`, `.cancel`, `.result`, `.accept`, and `.resume`. Each strict JSON request
includes matching `kind` and `request_id`; all except catalog include `project_alias`; the host checks independent project grants and
the submit profile/mode before contacting the runner. The runner derives the owner
from trusted configuration and must independently enforce owner/project scope.
Replies are typed version-1 envelopes and are checked for correlation, task/project
identity, digest/revision bounds, timestamps, pagination, event order and status codes.

Open **Agents** from Home, or `/agents` (redirecting in the same tab to `/#agents`).
The generated Rust/WASM view provides task composition, saved tasks, paged results,
cancellation, acceptance and explicit receipt-gated resume. A separately configured
coding worker executes the pinned profile in an isolated Git worktree; no worker
consults global registries. A missing Work service returns `503`; revoked profile
or mode grants return `403` before relay effects. Exact mutation retries return
their original business receipt. Acceptance marks review complete and never merges code.

The optional `usage` block supplies its own loopback `endpoint`, `token_file`,
`allowed_projects` and `allowed_profiles`, independently of Work and Chat. Bootstrap
stages `UsageRelayFactory`, then creates and injects its ready `Singleton` after
freeze. `ide.usage.overview`, `.detail` and explicit-profile `.refresh` serve bounded
metadata and coalesced refresh requests. Requests never invoke an agent or poll
accounts on the HTTP thread. A worker collector reports verified Codex allowance,
separate account activity and Forge job counters; Antigravity account allowance is
explicitly unsupported in headless mode. The API never exposes account identifiers.
Work and Usage remain disabled when their respective configuration is absent.

See [the complete contracts and worker setup](../../../docs/architecture/coding-admission.md)
and [the runner guide](../../../deploy/mission-control/agent-runner/README.md).
The host validates browser ABI7; deploy the matching generated bundle atomically.

Work requests are at most16 KiB on the wire, with an8 KiB UTF-8 prompt and2 KiB
acceptance criteria. Escaped JSON may reach the wire limit first. Lists default to20
tasks (maximum50), and event pages default to50 (maximum100). These admission and
transport bounds do not impose a model output-token limit or alter Chat/Mail budgets.

### Disposable real-agent fixture

```sh
cargo build --manifest-path src/rust/Cargo.toml -p forge-agent
cargo run --manifest-path src/rust/Cargo.toml -p forge-ide-web-host \
  --example loopback_fixture -- \
  /absolute/generated-ui /absolute/src/rust/target/debug/forge-agent
```

This test/demo creates a unique owned temporary project and ephemeral CA/client/
server keys, starts the **real agent**, and uses real mTLS. No system trust store or
external host is changed. `workspace` contains declaration hierarchy, a parse-error
partial report, a special-character filename and (Unix) a non-UTF-8 name. The
`denied` alias demonstrates the agent's separate authorization gate. Shutdown kills
and waits for the owned child and removes the owned fixture. Abrupt process kill
may leave a private temporary directory; it is not an operational credential store.
These native fixtures do not by themselves prove browser/WASM execution; browser
journeys must be reported separately.

## Browser boundary

### Optional HTTPS proxy origin

Add `"public_origin": "https://ide.example.com"` to the host JSON only when a
trusted TLS proxy already restricts access. The listener remains loopback HTTP.
The configured origin is exact and canonical: lowercase DNS hostname, no path,
credentials, query, fragment, trailing slash or explicit default `:443` port.
An optional nondefault port must match the browser. The host checks the original
`Host` and `Origin` and adds `Secure` to its existing HttpOnly/SameSite cookie.
It never derives origin or identity from `Forwarded`, `X-Forwarded-*` or Cloudflare
headers. Preserve original Host/Origin and Content-Length through the proxy.

When the TLS proxy already authenticates access, add
`"session_auth": "trusted_proxy"` alongside `public_origin`. The initial browser
Resume request then opens the Forge session automatically: no pairing code is
created or requested, and the Pair command is rejected. Cookies remain Secure,
HttpOnly and SameSite=Strict; inspection still requires its session cookie, CSRF,
epoch, target and project checks. Refresh resumes an active session or creates a
fresh session after expiry, without a server restart or manual code. The default
`"session_auth": "pairing"` retains the existing local pairing behavior.

Trusted-proxy mode requires an explicit HTTPS public origin and a loopback bind.
It trusts the local TLS proxy's access policy; it does not verify Cloudflare
Access JWTs or infer identity from forwarded headers. Keep upstream access controls
enabled and the backend inaccessible off-host. Local processes and SSH users are
inside this trust boundary. This remains one shared operator session, not isolated
multi-user identities or delegated authorization. An active session is preserved
when another admitted browser connects; expired credentials and epochs rotate.
The agent still checks its separate mTLS and project grants.
See [deployment and rollback](deploy/README.md) for service configuration.

### Request contract

- Fixed static `GET`/`HEAD`: `/`, `/index.html`, `/style.css`, `/main.js`,
  `/worker.js`, `/app.wasm`. The manifest is **not** served. Static responses carry
  CSP, `nosniff`, `no-store` and no-referrer policy. No catalog/credentials are assets.
- One command URL: **`POST /forge/v1/dispatch`**. Other methods receive `405` and
  `Allow: POST`; no fake REST verbs or per-task URLs.
- Exact `Host` and `Origin`, JSON Content-Type, canonical version/routing headers,
  single header values, typed body and header/payload agreement are mandatory.
- `ui.session.open`: one-use 256-bit random code in default pairing mode (five-minute
  lifetime, at most 20 attempts); denied in trusted-proxy mode. `ui.session.resume`:
  exact Origin plus a valid cookie in pairing mode, or automatic session admission
  through the explicitly trusted proxy in trusted-proxy mode.
- `ide.inspect`: cookie, session-bound `Forge-CSRF`, session epoch, configured
  node/target incarnation/project, expiry, and shared-bridge admission are checked
  **before** invoking the remote task.
- Session cookie is host-only, HttpOnly, SameSite=Strict,
  `Path=/forge/v1/dispatch`, default 30-minute fixed lifetime. The loopback HTTP
  prototype cannot claim encrypted browser transport or a Secure cookie. Local
  malware, hostile local users, browser compromise and stolen pairing secrets are
  outside its trust model. The default mode must not be exposed through proxies.
  For an access-controlled HTTPS proxy, explicitly set `public_origin` as below.
- Browser `UiEvent` replies are bounded typed projections. Native path names retain
  tagged encodings; report diagnostics/type spellings may expose source metadata.
  This is not secret redaction or a full compiler AST.

Required headers on every command: `Forge-Envelope-Version: 1`,
`Forge-Contract-Version: 1`, `Forge-Destination`, `Forge-Request-Id`.
Inspect additionally requires `Forge-Target-Node`, `Forge-Target-Incarnation`,
`Forge-Expires-Unix-Ms`, `Forge-CSRF`. Unknown `Forge-*` headers are rejected; there
is no browser principal header. Pair/resume have no target/expiry headers.

Bodies are tagged `pair {request_id, code}`, `resume {request_id}`, or
`inspect {request: PreparedInspection}`. Responses are
`{protocol_version:1,event:UiEvent,csrf?:string}`. CSRF stays in generated worker
transport plumbing, not the Rust domain model or DOM. HTTP failures use a bounded
`{error:...}` response; application errors can be typed events with HTTP 200.

## Workbench checkpoints and retired Chat/Mail compatibility

Chat and Mail reducers, browser dispatch codecs, host routes and generated UI
have been removed. Their PostgreSQL history is retained by the existing backend.
Legacy Chat/Mail links lead to Agents, and supported dormant tab snapshots keep
their drafts, identities and pending-run hints without activating those features.
Unknown historical top-level workspace `mail` values are ignored; the supported
separate Mail navigation snapshot remains inert and preserves its draft.

The legacy `chat` configuration key now selects only the optional loopback
Workbench workspace store. Its token remains native and outside served assets:

```json
"chat": {
  "endpoint": "127.0.0.1:18083",
  "token_file": "/etc/homelab-forge/web/agent-runner.token",
  "workspace_persistence": true
}
```

The session catalog always disables `chat_history`, `async_chat`, `mail_enabled`
and `mail_archive_enabled`, including when old configuration flags remain true.
Authenticated retired Chat/Mail commands fail before any upstream request.

The same-origin workspace endpoint at `/forge/v1/dispatch/workspace` requires the
active session cookie and CSRF token. With `chat.workspace_persistence` enabled,
the host forwards checkpoint requests to the configured local runner's
`/v1/workspace` endpoint. Restore and Save require explicit `preserve_chat: true`;
history hydration, model discovery and omitted/false preservation are rejected
before relay. The runner retains dormant Chat data while the Workbench bookmark
changes. The host revalidates that bookmark against an admitted catalog and a
fresh inspection. An older page lease cannot replace a newer checkpoint.

Without the runner setting, the host keeps up to 12 bounded tab checkpoints in
session memory. Session rotation and host restart clear those checkpoints.

## Bounds and limitations

Headers: 16 KiB/48 fields; request: default16 KiB (maximum64 KiB); response:
default/maximum8 MiB. Absolute accepted-connection budget includes reads, dispatch,
serialization and writes. Upstream expiry is **shortened**, never renewed, to the
remaining budget. Slow trusted synchronous work cannot be preempted, and an aborted
browser request does not prove the agent stopped. No automatic retry occurs.

HTTP/1.1 uses strict Content-Length only, closes every connection, and rejects
chunking, upgrades, `Expect`, duplicate headers and buffered pipelining. This narrow
adapter is intentionally not a general-purpose HTTP server. Asset manifest count
is exactly five; each asset is capped at16 MiB, hash verified and loaded via the
existing rooted native file client (no symlink traversal or shell wrappers).
The host build renders the portal home and Design reference from typed Rust
declarations in `forge-ide-web-host/src/design`. Open **Home** in the IDE or
visit `/home` for links to the IDE, Studio, the Room One playtest, and Design.
The host embeds these documents with their styles and diagrams. The Design
overview is a compact index with sticky topics and focused readers. `/design/implementation`
is the current source/configuration checkpoint; current controlled captures are in the
wireframe reader. `/design/characters` showcases Scout creation stages and authored
animation clips from the existing rig and illustrated skin. The sample
Previous chats sandbox is at `/design/previous-chats`, layout snapshots are at
`/design/wireframes`, and recovery notes are at `/design/workspace`. The chat
history contracts and sequence diagrams are at `/design/chat-history`.
The shared docs navigation returns to Agents or Workbench in the same tab;
the admitted worker applies the declared entry action after restoring the workspace. The PostgreSQL inference dispatch design and
sequence diagram are at `/design/chat-dispatch`. The design pages use fixed
asset paths.
The configured startup directory and generated content remain trusted; hashing
provides consistency, not a publisher signature or a filesystem snapshot.

Portable view bounds are stricter than native inspection:4096 entries,16384 total
declarations,4 KiB per text and2 MiB aggregate text. Over-limit projections fail
explicitly rather than truncating silently. WASM/UI bounds and agent traversal
bounds remain independent. Native parser work is trusted-source synchronous work,
not an adversarial-input sandbox. Operational credential provisioning, multi-user
identity/delegation, public TLS hosting and file editing are deferred.

## Browser playtest

Open **Design → Game engine design → Open browser playtest** to play Room One.
The host embeds its Rust-generated HTML, CSS and browser adapter at `/playtest`.
The player loads the verified `app.wasm` from the current UI bundle and runs its
own Rust game state and fixed simulation ticks in the tab. The browser draws the
board with Canvas 2D from one versioned WASM memory snapshot and maps keyboard
and on-screen direction controls to game actions. Room One includes a patrolling
guard. **Debug tools** adds pause, exact single-step, an inspector, frame timings,
and a bounded trace download. The export labels captures with missing movement
records or ring loss as incomplete for replay. Rust issues the typed live asset
and replay archive reads. One worker per page serves those requests, and Rust
verifies the returned buffers before admitting them. Rebuild the host and
regenerate the UI bundle together.

## Reproducible real-browser acceptance

The browser suite is test code, not hand-authored application JavaScript. It lives
in [`forge-ide-web/tests/browser`](../forge-ide-web/tests/browser/). With Node20+
and a supported Linux desktop/CI browser environment:

```sh
# Build the real WASM + generated bundle using the root UI targets first.
cargo build --manifest-path src/rust/Cargo.toml -p forge-agent --bin forge-agent
cargo build --manifest-path src/rust/Cargo.toml -p forge-ide-web-host --example loopback_fixture
cd src/rust/forge-ide-web/tests/browser
npm ci --ignore-scripts
npx --no-install playwright install --with-deps chromium
node journey.mjs \
  /absolute/generated-ui \
  /absolute/repo/src/rust/target/debug/forge-agent \
  /absolute/repo/src/rust/target/debug/examples/loopback_fixture \
  /absolute/browser-results
```

The pinned Playwright runner enables `chromiumSandbox: true`, preserves CSP and
certificate checks, and never intercepts/replaces fetch. It launches its own
fixture and browser in the **same execution namespace**. The real flow includes
pairing, generated WASM-worker inspection through native host/mTLS/agent, partial
warnings, file/declaration details, escaped/native names, keyboard focus, repeated
events, agent-denied alias, refresh and admitted session resume.

A **separate** fixture invocation with `--faults` replaces only the prepared
inspection port with a deliberately slow typed test implementation. This tests
local cancellation, selection-revision invalidation and timeout/late-completion
semantics through the real generated browser/WASM/host. It is not evidence of
remote transport behavior. The normal fixture never chooses that substitute.

Artifacts are screenshots taken only after the password field is cleared/masked
and a small scope-labeled result JSON. No traces, cookies, credential storage,
headers or pairing-code logs are saved. Assertions and process cleanup are bounded;
cleanup signals only the runner's newly created fixture process group.

A successful native `cargo test` or runner syntax check is **not** a browser pass.
If sandboxed Chromium cannot launch or a browser cannot reach the isolated
listener, report that boundary as blocked and run this suite in an approved
supported environment; never disable the sandbox, ignore TLS or publish a tunnel
as a substitute. Hosted CI outcome must be checked before calling journeys passed.

### Hosted browser selection

The browser workflow uses the Ubuntu runner's **already installed Google Chrome**
(`FORGE_UI_BROWSER_CHANNEL=chrome`) through Playwright's supported channel API,
with `chromiumSandbox: true`. Ubuntu's existing Chrome AppArmor profile permits
its sandbox; we do not edit AppArmor/sysctl settings, relocate executables or
disable sandbox/certificate checks. Without the environment selection, the same
runner script uses its locally installed Playwright Chromium; other values fail.
See [Playwright browser channels](https://playwright.dev/docs/browsers#google-chrome--microsoft-edge),
[Chromium's Ubuntu profile explanation](https://chromium.googlesource.com/chromium/src/+/main/docs/security/apparmor-userns-restrictions.md)
and the [GitHub runner inventory](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md#browsers-and-drivers).
The earlier hosted attempt failed at downloaded headless-shell sandbox startup,
before UI assertions; changing a supported browser selection is not a passing test.

### Workbench asset compatibility

The host accepts asset manifest ABI 13. Rebuild and replace the complete generated
bundle for the retired browser codecs; ABI 12 or unknown asset versions fail
before listening. Hashes bind the five files as a bundle, not publisher identity.
The native session/inspection HTTP protocol remains 1. See the
[local UI migration](../forge-ide-web/WORKBENCH.md#compatibility-and-verification).

The portal does not expose manual Mail archiving or any other Mail mutation.
Old configuration flags cannot enable these retired routes. Existing backend
archives and schema history remain intact. This retirement needs no database
deletion, schema rewrite or running-configuration change.

## Selected source reads

The host admits `ide.source@1` through the same command bridge, exact Host/Origin,
session cookie, CSRF, target and deadline gates as inspection. The browser submits
only the accepted report token and entry ID. The host retains one bounded accepted
inspection mapping to native path components and SHA-256; it never accepts a
browser path. Starting another admitted inspection replaces that mapping. A tab
whose report has been superseded must refresh before reading additional files.
No source survives host restart as cached authority.

The separately registered source client calls the mTLS agent, which independently
checks operation/project permission and revision. Both the client and host verify
returned bytes against the expected digest before exposing source to WASM.
`InspectionPort::source` is an explicit optional substitution seam; its default
denies reads, preserving inspection-only consumers. The native response is at most
1 MiB. The UI displays bounded windows (256 lines / 64 KiB) and safely renders text;
this is a read-only source viewer, with no save or execution route. Generated assets
and WASM ABI 5 must be replaced atomically with this host.
