# Forge IDE generated web application

The real browser application is built from **typed Rust components/macros and
validated YAML style tokens**, not handwritten application HTML, CSS or JS.
It is an inspection UI, not a source editor. The worker runs the actual compiled
[`forge-ide-ui`](../forge-ide-ui/README.md) Rust model through the bounded WASM ABI.
The [native host](../forge-ide-web-host/README.md) supplies protected selectors and
adapts inspection to the existing remote agent; native credentials stay there.

## Authoring and build boundary

- `src/generate/application.rs`: application structure and declarative action/header
  mappings using `component!`; no raw markup, scripts, styles or template escape.
- `ui/theme.yaml`: validated colors and bounded numeric tokens. Unknown fields and
  raw CSS/script fields are rejected.
- `src/generate/{component,html,css,javascript}.rs`: reusable audited generation
  primitives. These own web syntax, DOM/message plumbing and transport mechanics;
  they do not own IDE selection, report algorithms or request state transitions.
- `src/wasm.rs`: real worker ABI and owned buffers, not a dummy WASM blob.

`register_assets(id, AssetConfig)` stages a reusable `AssetFactory` with no I/O,
rendering or WASM execution. The consumer registers/freezes, calls the factory,
and can acquire the bundle through `Singleton`. The factory composes the existing
public template registrar/factory/provider for the semantic document shell.
Only typed component-emitted escaped markup enters its raw content slot.
The build CLI then writes a **new** directory; existing output is never overwritten.
No build code or generator runs inside the native web host.

`build-assets` is optional and disabled by default. The WASM facade's normal
profile depends only on the portable model and serialization. Template/bootstrap/
provider/YAML/hash dependencies belong to the native build feature; the native
host is a different package. No native filesystem/parser/TLS/runtime enters WASM.

## Run from repository root

Prerequisites: stable Rust, `wasm32-unknown-unknown`, Python3 and Node (for checks).
Install the Rust target explicitly in your development environment if needed:

```sh
rustup target add wasm32-unknown-unknown
make ui-build
# Use the printed ASSETS directory in your trusted host config.
make ui ARGS='serve --config /absolute/path/ui-host.json'
```

For a disposable actual-agent fixture with owned temporary files and ephemeral
credentials, follow [the host fixture instructions](../forge-ide-web-host/README.md#disposable-real-agent-fixture).
Open its printed loopback origin and enter the one-use console pairing code.
Never publish that code or put it in a URL. The prototype is single-local-operator,
not an internet service or a general relay.

After pairing: choose an allowed node/project, Inspect, expand the file tree,
select a Rust file, select a declaration and inspect its line/count/span metadata.
Partial reports remain visibly partial. Cancel stops local waiting and invalidates
late responses; it does not kill the agent's synchronous parser. Retry is explicit.
Refresh resumes the memory-only authenticated session but starts a fresh worker/model.
No file writes, source editing, process commands or remote jobs are exposed.

## Runtime ownership

The main thread only applies typed view bindings using `textContent`, semantic
controls and keyed rows; it owns focus, keyboard navigation, DOM and page/worker
lifecycle. No backend fetch or business reducer runs there. Worker glue executes
Rust's prepared effects, projects headers, performs bounded HTTP I/O, retains CSRF
transport metadata and returns typed results to WASM. Rust owns request tokens,
selection revisions, stale-result rejection, reports, tree expansion and details.
A WASM ABI failure closes that instance and aborts its pending fetches; transport
failures become recoverable typed model events. No automatic retry exists.

Main and worker generated files are generic control/transport primitives, not
application-authored scripts hidden in a Rust template. Tests generate a separate
counter component document with the same emitters. Extending application logic
means typed Rust events/model/components, not editing emitted assets.

## Verification and limits

```sh
make ui-check       # exact two-build asset comparison + syntax + real WASM + worker fault checks
make ci             # all native/profile/dependency checks plus ui-check
```

The public consumer tests import the real registrar, freeze, factory and provider;
CLI tests preserve existing output and reject malformed WASM. A header check only
recognizes WASM format/version; it does not certify the supplied program. The
actual build is exercised by the Node ABI harness. Generated-worker fault tests
use simulated HTTP/ABI failures and do **not** claim browser/network coverage.
Actual browser → generated worker/WASM → host → real agent journeys are a separate
acceptance gate, not inferred from string assertions or native endpoint tests.

Asset output is under ignored `src/rust/target/ui-assets`; committed sources remain
Rust/YAML. `manifest.json` records exactly five asset hashes; the host verifies
those bytes before listening. This detects drift, not publisher authenticity.
Host configuration/content, generated WASM and local operator are trusted. The UI
is not a secret-redaction system: identifiers, diagnostic/type spellings and native
filename encodings may expose project metadata. Source bodies are not an editor API.
