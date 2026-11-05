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
The configured startup directory and generated content remain trusted; hashing
provides consistency, not a publisher signature or a filesystem snapshot.

Portable view bounds are stricter than native inspection:4096 entries,16384 total
declarations,4 KiB per text and2 MiB aggregate text. Over-limit projections fail
explicitly rather than truncating silently. WASM/UI bounds and agent traversal
bounds remain independent. Native parser work is trusted-source synchronous work,
not an adversarial-input sandbox. Operational credential provisioning, multi-user
identity/delegation, public TLS hosting and file editing are deferred.

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

The host accepts asset manifest ABI 2. Rebuild and replace the complete generated
bundle for the report-keyed workbench; old ABI 1 or unknown asset versions fail
before listening. Hashes bind the five files as a bundle, not publisher identity.
The native session/inspection HTTP protocol remains 1. See the
[local UI migration](../forge-ide-web/WORKBENCH.md#compatibility-and-verification).
