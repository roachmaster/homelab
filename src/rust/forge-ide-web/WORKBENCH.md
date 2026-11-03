# Read-only Forge IDE workbench

The [approved wireframes](../../../docs/design/forge-ide-wireframes/README.md) and
[component decomposition](../../../docs/design/forge-ide-components/README.md) now
map to typed component declarations and a real worker/WASM model. This remains
**structural inspection**, not a source editor, compiler, terminal or debugger.
Native files, Rust declaration names/kinds/spans and supplied diagnostics are the
only report data. No new backend operation or per-task URL is introduced.

## Use the workbench

[Build and start the local host](README.md#run-from-repository-root), open its
printed loopback origin, and enter its one-use pairing code. The Connection screen
is replaced by the Workbench after an admitted catalog. Native mTLS keys never
enter the page; visible node/project choices do not confer authorization.

1. Choose an allowed **Node** and **Project**, then **Inspect**. Changing either
   clears the previous report. Loading disables duplicate Inspect and offers
   **Cancel local wait**; cancellation never claims the agent stopped computing.
2. Use **Explorer** to expand and select files. The dominant **File overview**
   shows the full safe name, metadata and declaration hierarchy. Select a
   declaration there or in **Outline** for its structural details.
3. Filter entry/declaration names locally. Queries are case-sensitive literal
   substrings, at most 256 UTF-8 bytes, against the displayed safe name. Matching
   spans the whole accepted bounded report before pagination; matching ancestors
   stay visible. This is not remote, source-content or full-repository search.
4. **Problems** lists the report's supplied diagnostic strings and links each to
   its file. It never invents severity, compiler success or source locations.
   Partial reports remain partial even when Problems is empty. Selecting a
   diagnostic shows that string with its file metadata.
5. Use **Previous page / Next page** for bounded local row windows, without a
   network request. A selected file hidden by a filter remains explicitly selected
   in the workspace. Clear the filter to return to ordinary tree browsing.
6. Hide/reopen supporting panes or reset their layout. At narrow widths use the
   visible pane switches; selection/report ownership stays in the same worker.
   Pane geometry and focus are in-memory presentation, not URL or persistent state.

Timeout, denied and unavailable outcomes clear old report data. Retry is explicit.
A generic Denied outcome cannot distinguish session expiry from operation denial;
**Reload to check session** creates a fresh worker and resumes admission, without
replaying Inspect. Fatal worker/ABI failure offers only fresh-instance recovery.

## Keyboard and lifecycle

- Native Tab reaches context controls, filters, panes and view controls. Tree
  Up/Down/Home/End moves focus, not selection; Enter/Space selects. Left/Right
  follows visible hierarchy/expansion. Focus and selected fill are distinct.
- Full names are available as safe text in a focused-row readout and selected-file
  details; an ellipsis/hover title is not the only way to read them.
- Focused separators expose their range and current size. Arrow keys resize,
  Shift uses a larger step, Home/End uses bounds, and Enter hides/reopens the pane.
  Visible controls and layout Reset provide non-drag alternatives.
- Hiding a focused pane moves focus to a visible reopen control. Narrow-pane
  switches preserve a valid last focus target or focus their heading. Changes in
  report ownership discard row focus keys instead of selecting a reused integer ID.
- Pagehide clears protected catalog/report DOM and terminates the worker. A
  restored page or reload starts a new instance, rechecks the host session and has
  no report. Fatal failure similarly clears protected content and stops dispatch.
  No History API, deep links, localStorage, report cache or cross-tab state exists.
- Filter inputs retain a bounded in-flight presentation draft while older worker
  views arrive. A presentation-only sequence acknowledges the exact input; the
  worker removes that envelope before Rust dispatch. Query semantics still live
  solely in WASM. A changed report/entry, disabled input or teardown clears drafts.

These contracts require actual browser acceptance in addition to pure-model tests.
Keyboard and narrow-window checks are not a claim of full screen-reader/mobile
certification.

## Implementation ownership

| Layer | Implemented responsibility |
| --- | --- |
| `generate/application.rs` and `ui/theme.yaml` | Declarative composition, labels/actions, bounded geometry and visual tokens; no authored HTML/CSS/JS |
| `generate/component.rs` and generic emitters | Reusable Screen, Toolbar, Filter, PaneLayout/Pane and Reload primitives; semantic DOM, geometry/focus, lifecycle and message plumbing |
| `forge-ide-ui::model` and private `model::projection` | Context, accepted report, selection/filter/diagnostic/page policy, bounded typed views and stale-event guards |
| Generated worker + `src/wasm.rs` | Real ABI 2 instance, bounded codecs, prepared effect execution and transport abort; no second business reducer |
| Existing native host/agent | Session/Origin/CSRF, configured aliases, mTLS and separate operation/project authorization, canonical shared bridge |

C01–C09 are design composition names, not nine public Rust classes or crates.
The existing public asset config/registrar/factory and provider integration is
retained. The default WASM dependency profile still excludes native filesystem,
parser, TLS and server runtimes. A non-IDE component fixture exercises the same
emitter; extending a generator primitive is not permission to hide app-specific
JS in it.

## Accepted report guards and bounds

The model retains the accepted `RequestToken { session_epoch, revision, request_id }`.
Every report-local action includes it: select, expand/collapse, filter, Problems,
and both pagination directions. Declaration/outline actions also carry the current
entry ID. Missing or old keys cannot mutate a new report even when it reuses ID 0.
Context changes, a new inspection, cancellation or teardown invalidate ownership.
The key is context, not authentication and not a stable external/deep-link ID.

Reports retain existing 4096-entry, 16384-declaration, 2 MiB aggregate-text and 8 MiB
codec limits. Each projected list uses at most 256 rows and a conservative encoded
byte budget; large rows can yield fewer than 256. Previous/Next advances by the
actual displayed window. There are no silently inaccessible rows behind a fixed
first-page cutoff; filters run before paging. No diagnostics are interpreted as
commands or markup. Native names remain explicit encoded values when necessary.

## Compatibility and verification

**Local ABI 2 is a coordinated experimental API change.** Existing public `UiEvent`
local variants now require report context (and entry context where applicable);
new view bindings/actions support filters and pane contents. Source callers and
serialized local-event producers must migrate. ABI 1 workers/WASM are rejected,
not silently mixed. Replace the five generated assets and their hash manifest
atomically; do not pair an old worker or main script with new WASM.

The browser/native protocol version remains 1: catalog, inspection request/report,
agent headers and `POST /forge/v1/dispatch` are unchanged. Core Task/IoClient,
provider/bootstrap and shared bridge contracts are unchanged. This statement does
**not** claim the public UI event/view API is source-compatible.

```sh
make ui-check   # deterministic generation + real ABI 2 + generated-worker faults
make ci         # native/profile/strict gates and ui-check
```

The hosted browser workflow uses standard installed Chrome with its sandbox
**enabled**. Its real-agent path and separately controlled fault path must both
pass, and desktop/narrow screenshots must be reviewed before main promotion.
Native, Node and mocked-fault checks never substitute for that real-browser gate.
The additional generated-main input harness uses a minimal simulated DOM boundary
to prove draft/ack ordering, not browser layout or real network behavior.
No deployment, operational credentials or user trust-store changes are needed.

## Workbench presentation refinement

The selected declaration appears directly below the selected entry heading, before
metadata and the mirrored declaration list. Metadata uses a compact, wrapping fact
grid. Explorer, Outline and Problems keep filters and paging outside their scrolling
tree; very short panes retain an outer scroll fallback so controls remain reachable.
Cancel is shown only while this browser owns a pending wait. A catalog with no
allowed project says so beside the disabled Inspect action.

These are generated presentation changes, not source editing or a new search,
transport or diagnostics capability. Partial/cancelled states use a labeled warning
tone; failures use a labeled error tone. This indicates request/report status, never
inferred compiler severity. Primary Inspect has a separate, contrasting keyboard
focus ring. Supporting text and disabled controls remain legible without competing
with the inspected entry.

Public component declarations now include `heading`, `caption`, `context`, `primary`
and `facts` macro forms. They share the existing audited emitters with other
applications. `TextStyle` and `ButtonEmphasis` are bounded Rust enums, not arbitrary
CSS class strings. Existing text/button macros keep their meaning. Direct Rust
`Component::Text` and `Component::Button` constructors must supply the new `style`
and `emphasis` fields; this is not Rust source compatibility for those constructors.

`TextBinding::tone` is additive serialized ABI 2 view metadata. The current renderer
accepts only the four declared values and defaults missing/unknown values to neutral;
older renderers ignore this field. Rust consumers constructing `TextBinding` must
supply `Tone`. No input event, request-token guard, WASM memory export or HTTP schema
changes. Deploy all generated assets and WASM atomically as before. The browser gate
covers the stylesheet cascade, primary keyboard focus, selected-declaration placement,
scroll ownership, short-pane control access and state changes. Static/model tests are
not a substitute for that real-browser evidence.
