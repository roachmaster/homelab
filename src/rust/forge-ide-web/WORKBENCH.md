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
2. Use **Explorer** to expand and select files. The dominant **File** pane
   shows the full safe name and declaration hierarchy. Selecting a declaration shows its details before the declaration list, preserving a direct route to another declaration. Entry metadata and Source shape inventory are keyboard-accessible disclosures, closed by default. Report totals and detailed limits live in Inspection details. Select a
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
6. Use the visible **Explorer**, **Outline** and **Problems** controls to hide/reopen supporting panes or reset their layout. Outline and Problems are mutually exclusive; reset restores Explorer and Outline. At narrow widths use the
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
tree. Supporting-pane full-name readouts reserve stable space so focusing a row
cannot move its pointer target before click. Same-order list projections do not
move existing row nodes, preserving independently scrolled trees. Very short panes retain an outer
scroll fallback so controls remain reachable.
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

## WASM orchestration and a future editor host

### Implemented display contract

The orchestrator is the **Rust/WASM model running in the browser worker**, not a
JavaScript controller or the remote agent. The current flow is:

1. Generated main-thread controls send a typed `UiEvent` to the worker. The worker
   adds the clock and passes bounded `Input` bytes through the WASM ABI.
2. `UiModel::dispatch` applies application policy and produces
   `Output { view: ViewModel, effects, ... }`. The generated worker posts the view
   to the main thread and executes only declared typed effects. Transport results
   return through WASM before they can change the accepted view.
3. Generated main-thread primitives display the `ViewModel` bindings: texts,
   choices, buttons, inputs, lists, tables, visibility and pane context. DOM,
   focus, scrolling, pane geometry and bounded in-flight input drafts stay here;
   selected content, accepted revisions and request policy stay in WASM.

See the actual [protocol](../forge-ide-ui/src/protocol.rs),
[view bindings](../forge-ide-ui/src/view.rs),
[model](../forge-ide-ui/src/model.rs), [WASM ABI](src/wasm.rs) and
[generated adapters](src/generate/javascript.rs).
`ReportView`/`EntryView` contain inspection metadata and declarations, **not source
bodies**. There is currently no source read/write operation, editor binding or
editor lifecycle event. Selecting a file therefore displays its structural
overview, not an editable document.

### Proposed, unimplemented editor-host lifecycle

A JavaScript editor widget can fit this boundary: WASM requests its display through
a future typed view binding, and a reusable generated main-thread host mounts it.
The following is a design contract, **not an existing API or a runtime addition**:

- **Mount:** after accepting an authorized source-read result, WASM projects a
  bounded editor binding identifying the mount region, document/context, editor
  instance generation, source revision, text and permitted editing mode. A typed
  component declaration supplies the mount region. The host creates or updates
  the widget idempotently and reports mount success/failure as a typed event.
  Neither the widget nor the main thread fetches source or chooses the document.
- **Change:** the widget emits bounded, sequenced text-change events with the
  document identity, instance generation and base revision. WASM checks ownership
  and ordering, accepts or rejects edits, owns dirty/conflict/save policy and
  projects the acknowledged revision. The widget may retain a presentation buffer,
  caret, selection and composition state; an older acknowledgment must not erase
  newer local input. Programmatic updates must not echo as fresh user edits.
- **Save:** only WASM may prepare a future typed write effect. Generated worker
  I/O uses the admitted gateway; the host/agent independently authorize the
  operation/project and enforce the expected source revision. A saved state
  requires an accepted write result, not a widget callback or an optimistic label.
- **Unmount:** WASM withdraws/replaces the binding when document ownership ends.
  The host disposes listeners/widget buffers and returns focus appropriately;
  late callbacks/results from that instance cannot affect a new document. A
  hidden pane alone does not decide document closure or discard unsaved changes.
  Pagehide, session reset and fatal recovery immediately clear protected editor
  content and dispose the widget even if WASM cannot respond. Local cancellation
  does not establish whether a remote write completed.

Source read/write DTOs, revision/conflict semantics, bounds, editor bindings and
events, the generated host primitive and their ABI compatibility are all **future
work**. They require model/codec tests and real-browser lifecycle, composition,
focus and stale-message checks before delivery. This refinement adds neither a
fake editor nor placeholder APIs. Application UI remains typed Rust/YAML with
generated assets; integrating a widget does not authorize a JavaScript business
reducer, arbitrary backend routes or handwritten application HTML/CSS/JS.


## Ergonomic layout declarations

The `disclosure` macro emits a native `details`/`summary` control with escaped labels
and normal keyboard behavior. Its contents still come from accepted WASM view
bindings and are cleared on pagehide/fatal recovery, even when collapsed. Collapsing
is local presentation and does not issue an inspection. No application policy or
source-body data is inferred from DOM state.

The `focused_layout` macro declares a single-secondary presentation policy: the
leading pane and main work area remain independent while trailing and bottom tools
are mutually exclusive. Initial/reset layout opens the trailing tool and closes
the bottom tool. Existing `layout` retains independent supporting panes. Desktop
tool switches toggle visibility, narrow switches display one pane, and Reset and
keyboard separators remain available. The policy changes only geometry and focus;
accepted project/file/declaration state remains in WASM.

`Component::Disclosure` is a new public enum variant and `Component::PaneLayout`
now requires `single_secondary` for direct Rust constructors. Exhaustive consumers
must migrate. `RowView::icon` is additive ABI 2 presentation metadata with bounded `RowIcon`
values: Symbol, Document and Folder. The generic renderer falls back to Symbol for
missing/unknown values; older renderers ignore the field. Direct Rust RowView
constructors must supply the icon. Only Rust chooses the icon; domain kind labels
are never interpreted as generator style instructions. No UI event, memory export
or HTTP schema was added or changed. Generate the matching assets/WASM atomically as before.
