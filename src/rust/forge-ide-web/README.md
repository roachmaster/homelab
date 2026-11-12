# Forge IDE generated web application

The real browser application is built from **typed Rust components/macros and
validated YAML style tokens**, not handwritten application HTML, CSS or JS.
It opens to Chat after admission and includes a read-only source workbench. The worker runs the actual compiled
[`forge-ide-ui`](../forge-ide-ui/README.md) Rust model through the bounded WASM ABI.
The [native host](../forge-ide-web-host/README.md) supplies protected selectors and
adapts inspection and bounded source reads to the remote agent; native credentials stay there.

The [workbench guide](WORKBENCH.md) describes the compact toolbar, report-keyed
project-wide file/symbol search, numbered source windows, Outline, Problems and
reversible keyboard/narrow panes. Chat uses the local ABI 4 bundle.

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

After admission, use **New chat** to start a separate conversation and use
**Conversation** to switch between histories and drafts. Follow-ups send the
selected conversation's bounded history to the active Forge llama.cpp model.
Conversations live in the current browser tab's worker and clear on reload.
A failed send leaves the draft ready to retry.

Use **Workbench** to open the source tools. A catalog containing one allowed
project opens it automatically.
With multiple projects, choose the intended node/project and use **Refresh project**.
Use **Find files or symbols** to search names throughout the loaded report, then
select a result to read its source. Explorer file selection and Outline declaration
selection use the same checked source path. Narrow screens open the selected file
pane automatically and provide **Back to files**.

Source is read-only, bounded and checked against the bytes used for inspection.
A changed, unavailable, non-text or oversized file produces an explicit status;
it is never presented as verified source. Numbered windows and **Previous lines /
Next lines** keep the DOM bounded. Selecting a declaration opens and highlights
its source range. Search is over names in the loaded report, not full source text,
resolved references or an unbounded filesystem index.

Partial reports remain visibly partial. Cancel stops local waiting and invalidates
late responses; it does not kill the agent's synchronous task. Retry is explicit.
Refresh resumes the memory-only authenticated session and opens a sole allowed
project again. No file writes, editing, process commands or remote jobs are exposed.

## Runtime ownership

The main thread only applies typed view bindings using `textContent`, semantic
controls and keyed rows; it owns focus, keyboard navigation, DOM and page/worker
lifecycle. No backend fetch or business reducer runs there. Worker glue executes
Rust's prepared effects, projects headers, performs bounded HTTP I/O, retains CSRF
transport metadata and returns typed results to WASM. Rust owns request tokens,
selection revisions, stale-result rejection, accepted report keys, local filters,
bounded row/source windows, diagnostic navigation, tree expansion and details.
Page teardown/resume and fatal recovery clear protected DOM. A WASM ABI failure
closes that instance and aborts its pending fetches; transport
failures become recoverable typed model events. No automatic retry exists.

Main and worker generated files are generic control/transport primitives, not
application-authored scripts hidden in a Rust template. Tests generate a separate
counter component document with the same emitters. Extending application logic
means typed Rust events/model/components, not editing emitted assets.

## Verification and limits

```sh
make ui-check       # exact two-build asset comparison + syntax + real WASM + worker fault checks
make ci             # all native/profile/dependency checks plus ui-check
make ui-qa          # separate actual headless Chrome gate; see prerequisites below
```

The public consumer tests import the real registrar, freeze, factory and provider;
CLI tests preserve existing output and reject malformed WASM. A header check only
recognizes WASM format/version; it does not certify the supplied program. The
actual build is exercised by the Node ABI harness. Generated-worker fault tests
use simulated HTTP/ABI failures and do **not** claim browser/network coverage.
The generated-main input harness similarly uses a minimal DOM boundary to test
in-flight draft ordering; it is not a browser substitute.
Actual browser → generated worker/WASM → host → real agent journeys are a separate
acceptance gate, not inferred from string assertions or native endpoint tests.
The [headless QA guide](tests/browser/README.md) gives the single root command,
scenario inventory, safe JSON/Markdown/JUnit reports and sandboxed hosted workflow.

Asset output is under ignored `src/rust/target/ui-assets`; committed sources remain
Rust/YAML. `manifest.json` records exactly five asset hashes; the host verifies
those bytes before listening. This detects drift, not publisher authenticity.
Host configuration/content, generated WASM and local operator are trusted. The UI
is not a secret-redaction system: identifiers, diagnostic/type spellings and native
filename encodings may expose project metadata. Displayed source remains read-only; no editor/save API is exposed.
