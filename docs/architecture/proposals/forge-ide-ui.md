# Forge IDE UI: implemented browser boundary

**Current repository reference — reviewed 19 November 2026.** The original proposal was implemented and extended. Use the [implementation status](../../design/implementation-status.md), [application guide](../../../src/rust/forge-ide-web/README.md) and [Workbench contract](../../../src/rust/forge-ide-web/WORKBENCH.md) for current behavior. The retained [PDF](forge-ide-ui.pdf) and M19–M24 diagrams are historical proposal snapshots, not current authoring or API instructions.

## 1. Current application

The IDE at `/` has Chat, capability-gated Mail, and Workbench screens. Home (`/home`) and Design (`/design`) are same-tab documents. Design is a compact topic index with focused readers, a sticky topic bar and a shared global navigation shell. The Forge wordmark opens Home. `/studio` and `/playtest` are separately generated tools; their availability and execution boundaries are described in their own designs.

Chat uses a thread-title drawer, a new-chat icon, an open message stream and an inline composer. Management and saved-history search live in the drawer. Mail uses inbox rows, Compose, a thread reader and quick reply. Workbench places node/project and View options in header sheets, uses one search field with an entry-name mode, and gives source the primary workspace. Source has line windows, Wrap, and Markdown Raw/Display. Problems are supplied inspection diagnostics.

Contextual questions, staged edit proposals, direct text editing and guarded managed writes are implemented. They require verified source and the appropriate native capability. The separately dated 18 November live source checkpoint was read-only; deploying a generated control does not grant writes.

## 2. Cohesive packages and ownership

| Owner | Actual implementation | Responsibility |
| --- | --- | --- |
| Portable model | `forge-ide-ui`: model, protocol, view, validation and Markdown | Rust application state, prepared effects, request/report/source guards, Chat/Mail policy and edit lifecycle. No DOM, TLS or native files. |
| Browser/build adapter | `forge-ide-web`: generate, wasm, codec, playtest | Typed component/YAML authoring, generated presentation and worker transport, bounded real-WASM ABI. Build-only dependencies stay outside the browser profile. |
| Native host | `forge-ide-web-host`: config, service, HTTP, assets, relay and edit modules | Session/Origin/CSRF admission, selected providers, shared typed dispatch, source verification and configured Chat/Mail/Studio relays. |
| Source agent | `forge-agent` and `forge-ide` | mTLS identity, independent operation/project authorization, rooted inspection and separately opted-in managed writes. |
| Mission Control services | Chat/Mail runner and Studio service | PostgreSQL persistence, queued jobs, leases/recovery, inference or build execution and bounded results. These are distinct service implementations. |

Bootstrap stages effect-free registrars, freezes the typed plan, creates selected factories and injects ready ports. Runtime consumers receive scoped providers only when acquisition is needed. Registries stay at composition roots; pure UI values/reducers do not receive empty registrars.

## 3. Admitted request and result flow

1. A generated control sends a bounded typed intent to worker/WASM. Rust checks the current context and prepares an effect.
2. Generated worker I/O serializes the approved effect to the single same-origin `/forge/v1/dispatch` gateway. HTTP routing headers and the body variant must agree.
3. The host checks session, request origin, CSRF, destination, deadline and project before invoking the selected typed binding/client.
4. A source operation reaches the mTLS agent, which checks its own identity, operation and project policy. Chat/Mail reach the configured local runner; Studio reaches its own configured relay.
5. Rust accepts only correlated, bounded current results. Late reports/source, old search modes and stale edit revisions cannot overwrite current state. Main JS presents the accepted view.

Local filtering, pane switches and line paging do not issue inspection requests. Opening source may issue a separate admitted read. Cancellation ends local ownership and may abort transport; it is not proof that remote computation stopped. PostgreSQL queued inference is separate from synchronous source inspection.

## 4. Versioned contracts and state

The repository browser/WASM ABI is **5**, and the native HTTP protocol remains **1**. The separately dated 18 November live portal checkpoint used ABI 4; this source update does not deploy or enable the new capability. Replace the five generated assets and matching WASM atomically with a verified manifest. The source agent and its configured feature set have an independent release lifecycle.

Manual Mail archive requires the native `chat.mail_archive_enabled` opt-in (false by default), the matching runner/schema migration 0008, and coordinated host/ABI 5 asset rollout with session rotation. The terminal operation records a Manual reason and replays a matching owner/thread/base receipt without advancing its revision again. Older readers cannot decode that reason, so rollback after Manual records exist requires a compatible reader. See [host compatibility](../../../src/rust/forge-ide-web-host/README.md#workbench-asset-compatibility).

Report-local integer IDs are not filesystem authority or stable bookmarks. Prepared source requests carry accepted report context; source bytes must match the inspected digest. Managed writes additionally require native revision fencing and a durable operation receipt. Unknown write outcomes remain reconcilable rather than being reported as a successful save.

## 5. Build output is not runtime behavior

Application authors declare structure/actions in Rust (`component!`) and styles in validated YAML or Rust property maps. The established generator emits semantic HTML/CSS and minimal reusable JS glue. Docs use typed Rust nodes and bounded safe Markdown rendering. Generated output is packaged separately from authored source.

Hand-authored application HTML, CSS, JS, inline handlers or raw-code blobs are not the authoring path. Low-level audited generator primitives own web-language syntax; that does not move business policy into JS strings. Browser-native focus, scroll, modal presentation and lifecycle remain main-thread responsibilities. Native I/O and credentials remain native.

The standalone Studio browser controller is currently a generated-JavaScript exception: its Rust/WASM application-policy migration remains pending. Do not describe that desired ownership as already shipped. The IDE and Room One worker boundaries must be assessed from their actual adapters.

## 6. Navigation, persistence and recovery

Before opening Home/Design, the IDE saves its accepted workspace and a bounded local JSON fallback. Saved Chat history and queued runs are in PostgreSQL; the tab checkpoint holds drafts and a Workbench bookmark. Mail navigation/drafts have a separate same-tab handoff cache and are not a durable cross-device draft feature.

Docs links to `/#chat`, `/#mail` or `/#workbench` select declared existing Rust actions after session and checkpoint restoration. The generated adapter consumes the fragment afterward. Source bookmarks are reauthorized and re-read; report IDs and credentials are not placed in URLs. Browser Back retains normal browser navigation with leave guards where supported.

`pagehide`, context loss and fatal recovery clear protected DOM before a fresh worker/session check. Native browser warnings have browser-controlled text and behavior. A local checkpoint is a recovery aid, not an authorization token.

## 7. Current verification and remaining work

`make ui-check` checks deterministic assets, JS grammar, real WASM and controlled worker/presentation contracts. `make ui-qa` runs the fixed 28-group sandboxed Chrome suite, including real host→mTLS-agent flows and separately labeled controlled Chat/Mail/managed-write fixtures. Native tests, diagram validation and browser screenshots establish different evidence.

Remaining work includes the Studio browser Rust-policy migration, enabling/configuring services and managed writes where intended, and any future LSP/general editor, arbitrary tool execution or multiuser delegation. The current textarea editor and static-web Studio build do not imply those larger capabilities. See the [current status matrix](../../design/implementation-status.md) and [roadmap](../../roadmap/forge-framework.md).
