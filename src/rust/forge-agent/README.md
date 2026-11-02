# Forge agent: configured node and project inspection

**Implemented, read-only, synchronous first slice.** Two real processes are tested
on localhost with temporary credentials. The application can explicitly bind a
non-loopback address, but no two-physical-machine deployment, trust provisioning,
firewall change or production hardening is claimed. It is not a shell, editor,
process runner, remote job manager or operating-system kernel.

## Run

From the repository root (dependencies fetched as in the workspace README):

```bash
make agent                            # help: no config/project/network access
make agent ARGS='serve --config /path/to/server.json'
make agent ARGS='node.info --config /path/to/client.json --node lab'
make agent ARGS='inspect --config /path/to/client.json --node lab --project workspace'
# Or after cargo build --manifest-path src/rust/Cargo.toml -p forge-agent:
src/rust/target/debug/forge-agent --help
```

Cargo/Make diagnostics go to stderr. Client success writes one complete YAML
report to stdout; errors are nonzero with no partial report generated. A failing
stdout can still accept a prefix before returning an OS write error; output is
not a transactional file write. Duplicate/unknown options and malformed aliases
are rejected. `inspect` maps to the canonical `ide.inspect@1` operation.
`node.info` returns configured node identity and compiled OS/architecture, not a
health probe or hardware inventory. Reports preserve the existing IDE semantics,
including `complete: false`; successful transport does not imply complete inspection.

## Explicit startup configuration

JSON is a **startup boundary format**, deserialized into typed configuration;
internal requests/results remain typed enums and structs. No environment search,
URL guessing or service-locator lookup. Paths below are illustrative; supply your
own already-provisioned certificates. The program never generates credentials,
installs trust roots, edits permissions or deploys a host. DER files contain an
X.509 chain, PKCS#8 private key and trusted CA roots. Protect key/config files with
host permissions. Relative file and project paths resolve against the config
file's parent, not a remote request. Project roots cannot contain `..`.

Server:

```json
{
  "mode": "server", "bind": "127.0.0.1:9443", "allow_non_loopback": false,
  "node": "lab-node", "incarnation": "run-2026-11-02",
  "credentials": {"chain": ["server.der"], "key": "server.key.der", "roots": ["ca.der"]},
  "peers": [{"certificate": "reader.der", "subject": "developer", "node_info": true, "projects": ["workspace"]}],
  "projects": [{"alias": "workspace", "root": "/absolute/trusted/project"}],
  "limits": {"request_bytes": 4096, "response_bytes": 4194304, "timeout_ms": 5000, "poll_ms": 100}
}
```

Client:

```json
{
  "mode": "client",
  "credentials": {"chain": ["reader.der"], "key": "reader.key.der", "roots": ["ca.der"]},
  "nodes": [{"name": "lab", "address": "127.0.0.1:9443", "server_name": "localhost", "node": "lab-node", "incarnation": "run-2026-11-02"}],
  "limits": {"request_bytes": 4096, "response_bytes": 4194304, "timeout_ms": 5000, "poll_ms": 100}
}
```

`server_name` must match the server certificate; the socket address is configured
separately. Choose a fresh incarnation for a replacement host. TLS verifies the
certificate chain/name; the exact verified peer leaf must also match a configured
subject. Subject allowlisting is **not** operation authorization. Each subject
separately receives `node_info` and an explicit set of project aliases. No peer
header can grant identity. Unknown and forbidden aliases both return forbidden,
without revealing root/existence. The caller cannot supply a root or expand limits.
An empty project list supports node-only serving with the same registry type.

## Public composition and dependency direction

- [`forge_ide::assembly::register_projects`](../forge-ide/src/assembly.rs) validates
  a whole runtime-length alias catalog and stages **one `ProjectFactories` group**.
  It does not loop a type-growing `Registry<G>` at runtime, read roots or construct
  instances. `register_ide` is unchanged for individual consumers.
- [`host`](src/host.rs) registers node, project and gateway factory groups, consumes
  `freeze`, explicitly constructs instances, wraps them in `Singleton`, wires policy
  and bridge routes, then opens/publishes the listener. A startup failure drops
  previously created owned handles and publishes no `READY`/partial host.
  Startup config/certificate reads are boundary I/O before registration; configured
  roots are opened during factory creation, not during unauthorized requests.
- [`dispatch`](src/dispatch.rs) owns the finite `AgentCommand`/result sums. Verified
  transport context feeds its shared bridge. Target, expiry, header route/version,
  operation permission and payload kind are checked; the inspect handler checks
  project permission **before** invoking the existing `InspectTask<NativeFiles>`.
- [`forge_ide::remote`](../forge-ide/src/remote.rs) owns the optional typed
  `RemoteIdeClient`, `RemoteInspectTask`, `InspectCall`, `InspectReply` and report
  validator. The task prepares a typed call and validates the response. It receives
  no endpoint/config-file lookup or wire JSON. `Task` itself is unchanged.
- [`forge_remote_https::gateway`](../forge-remote-https/src/gateway.rs) owns generic
  `GatewayCodec`, bounded serving and an externally unconstructible
  `VerifiedContext`. Its optional [`ide`](../forge-remote-https/src/ide.rs) module
  owns private report DTOs and `HttpsIdeClient`, selected via a public registrar.
  The adapter imports capability contracts, never the application.
- The CLI's own commands/help use a local bridge binding; local admission is not
  network authentication. Output uses a narrow `IoClient<Vec<u8>>`. All remote
  calls still pass destination authorization. The local IDE CLI stays local.

Default IDE consumers omit remote/YAML/HTTPS; default HTTPS consumers omit the
IDE/parser. Only this application selects both. Public consumer fixtures import
real capability exports; no example-private registrar or `include!` workaround.
No core package acquires Tokio, TLS, JSON, parser or application dependencies.

## One authenticated front door

Both calls are **POST `/forge/v1/dispatch`**. Header destination chooses the
registered operation, not a URL/controller per task. Existing bounded singleton
`Forge-*` headers carry target/incarnation, contract version, request/correlation
IDs and absolute expiry. They cannot override TLS identity. Strict payload DTOs
are only `node.info` or `ide.inspect` with a validated alias; header/payload/version
mismatch and unknown fields reject before effects. Unsupported methods return
405 with `Allow: POST`; GET/PUT/PATCH/DELETE are not implemented.

This is a command gateway over HTTPS, not resource-oriented REST. See the
[wire contract](../forge-remote-https/README.md) and
[single-front-door skill](../../../.agents/skills/design-forge-front-door/SKILL.md).

## Bounds and lifecycle

| Bound | Current policy |
| --- | --- |
| Startup JSON / DER file | 1 MiB / 16 KiB each; 1..8 chain/root files |
| Catalog | 0..128 projects; 1..32 subjects; 1..128 named client nodes |
| Gateway request / response | default 4 KiB / 4 MiB; configurable hard caps 64 KiB / 16 MiB |
| Connection / accept polling | default 5 s / 100 ms; maximum 30 s / 1 s |
| Serving | one synchronous owner, one accepted connection/request at a time; no task queue or per-request thread |
| Report acceptance | bounded tree/declaration/string/source-byte counts; [exact defaults](../forge-ide/src/remote.rs) |
| Source inspection | existing [IDE traversal/parser limits](../forge-ide/README.md), `.git`/`target` excluded |

For the IDE client, `response_bytes` caps the received report independently of
`request_bytes`. The preserved node-only client uses its legacy symmetric
`max_body_bytes=request_bytes` cap for request and response (default 4 KiB);
`response_bytes` does not change that node-client limit. Server gateway
`response_bytes` caps both operation responses.

`READY address` is emitted only after assembly/listen succeeds. SIGINT/SIGTERM
(and supported SIGHUP via `ctrlc` termination handling) set a stop flag. Admission
rechecks it immediately before dispatch. Idle accept polling permits stop without
a request. Already-admitted synchronous native I/O/parser work may finish late;
there is **no hard shutdown grace bound or forced cancellation**. `STOP REQUESTED`
is best-effort acknowledgement, not proof the active callback ended. `STOPPED`
marks the return path which releases the owned listener/runtime. Optional
`--max-connections N` is a bounded test/demo lifecycle; rejected accepted
connections count, idle polls do not. A slow peer can occupy this one-owner server;
there is no availability guarantee or production rate limiter.

Construct, use **and drop** the HTTPS objects on their synchronous owner, not inside
an unrelated async runtime. The private Tokio runtime does not make the bridge or
capability tasks Send/Sync. `ctrlc` owns one process signal thread, not a request
executor. [Signal library contract](https://docs.rs/ctrlc/3.5.2/ctrlc/).

Original expiry/deadline is preserved across network read, decode and validation;
post-operation checks can report late completion, not interrupt work. A timeout
may mean inspection executed; no automatic retry, exactly-once guarantee, durable
acceptance or resume checkpoint. Bounded byte buffers prevent unbounded transport
output, not exact process-memory accounting. The independent 96-container wire-depth cap can reject a report within individual
tree/syntax-depth limits. Source inspection and report decode
are synchronous trusted-project work, not hostile-source sandboxes. Reports can
expose names, type spellings and diagnostics; they are not secret-redacted or an
atomic filesystem snapshot. No remote filesystem mutation or shell execution.

## Verification and next boundary

`make ci` includes actual binary tests spawning server/client children on localhost,
with temporary CA/leaf credentials and owned source fixtures. Cases include a
byte-for-byte local/remote YAML comparison, wrong server/client identities,
operation/project denial, target/incarnation mismatch, unknown node, unavailable
or stalled peer, response limits, invalid catalog, rejected-request continuity,
max-connection exit and signal-driven listener release. Direct tests count zero
capability calls after admission/variant/resource rejection. No private keys are
committed. This demonstrates two processes, **not two physical machines**.

A real two-host trial still needs explicitly authorized credential distribution,
network exposure and host controls. Remote jobs, delegation, writes, process
launch, additional HTTP methods and alternate carriers remain later decisions.
