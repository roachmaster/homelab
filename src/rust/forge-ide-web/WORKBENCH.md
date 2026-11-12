# Read-only Forge IDE workbench

The generated workbench lets an admitted user find a file or Rust declaration and
read its actual source. Application structure and styles are typed Rust components
and validated YAML; the compiled Rust/WASM model owns project, selection, source
and request policy. It does not provide editing, compilation, a terminal or debugging.

## Use the workbench

[Build and start the host](README.md#run-from-repository-root), then use its configured
entry method. A local host can require its one-use pairing code; a trusted proxy
host uses the proxy's existing sign-in. Agent mTLS keys never enter the page.

1. A catalog with exactly one allowed project opens it automatically after admission.
   Otherwise choose **Node** and **Project**, then **Refresh project**. Changing
   context clears previous report and source ownership. Refresh replaces the loaded
   report; it does not claim the filesystem remained unchanged since the last read.
2. **Find files or symbols** searches file paths and declaration names in the loaded
   report, case-insensitively. Results show their file context. Selecting a symbol
   opens its file and declaration directly. Clear the query to return to Explorer.
   A bounded report or partial inspection limits the search coverage; this is not
   source-text search, resolved references or a full-repository index.
3. Explorer shows the native file hierarchy. Selecting a text file requests its
   bounded source through the admitted host/agent path. Source must match the
   inspected content digest. Changed, unreadable, oversized and binary files receive
   explicit states, rather than stale source with apparently valid line spans.
4. The dominant **File** pane shows read-only source with line numbers. **Outline**
   provides declaration navigation. Selecting a declaration highlights its lines
   and opens the corresponding source window. Previous/Next lines traverses the
   accepted document locally; no additional file read occurs for line paging.
5. On phones, selecting a file or declaration reveals the File pane. **Back to files**
   returns to Explorer. Pane switches still expose Outline and Problems. Selection
   remains in the same model and survives presentation-only pane changes.
6. Metadata, source-shape inventory and inspection details remain supporting
   disclosures. Problems shows supplied inspection strings, without invented
   compiler severity, test success or source locations.

The existing Explorer filename and Outline declaration filters remain literal,
case-sensitive local filters. The global Find query is case-insensitive; all query
inputs are limited to 256 UTF-8 bytes. No regular-expression evaluation is exposed.

## Source and request ownership

A source request carries its own request token, the accepted report token, configured
node/project and report-local entry ID. Browser IDs are not native paths or authority.
The host and agent independently enforce admission. The portable model accepts a
source result only for its current report, file and pending request. A stale reply
cannot replace a later file or a refreshed project's source.

The accepted source document is bounded to 1 MiB. WASM projects at most 64 KiB and
256 lines per source window, retaining exact UTF-8 slices. A very long line can
continue across windows; the range text identifies continuation and the gutter
uses a continuation marker. Columns use Unicode character indexes matching source
spans. Selection highlights are line ranges, not a claim of semantic type resolution.
The source digest establishes correspondence with inspected bytes; it is not a
Git commit, atomic repository snapshot or publisher signature.

The generated main-thread source primitive renders only safe text nodes and line
numbers. It does not read files, choose the document, paginate source, interpret
markup or maintain application request state. Identical source bindings preserve
DOM nodes and horizontal scroll. Changed windows reveal their target lines.

Cancellation stops this browser's ownership and aborts local transport where
possible; it does not prove agent task cancellation. Source errors retain the
inspection context with an actionable status. Report/context changes invalidate
accepted source. Session reset, pagehide and fatal recovery synchronously clear
protected source and metadata DOM before starting a new worker.

## Keyboard, focus and responsive layout

Native Tab reaches context controls, search, panes, source windows and controls.
Tree Up/Down/Home/End changes focus; Enter/Space selects. Left/Right follows the
visible hierarchy. Focus and selection have separate visual treatments. Complete
names remain available as safe text in focused-row readouts and selected details.

Separators expose their range and size. Arrow keys resize, Shift uses a larger
step, Home/End reaches bounds and Enter hides/reopens a supporting pane. Visible
controls and Reset provide alternatives to dragging. Outline and Problems are
mutually exclusive in the focused layout. Narrow controls and rows use the validated
44 px touch token; this is not a claim of complete accessibility certification.

The model supplies an opaque pane reveal revision when an accepted action should
reveal a result. The renderer compares it to the previously consumed revision.
Repeated background views therefore cannot undo a subsequent manual pane switch,
while repeating a file selection can deliberately reopen the same File pane.

Report/entry context changes discard obsolete row focus keys. Filter inputs retain
bounded in-flight presentation drafts until the matching worker acknowledgment;
an old view cannot erase newer typing. A new context, disabled input or teardown
clears the draft. No History API, deep links, localStorage, persistent report cache
or cross-tab synchronization exists.

## Authoring and runtime boundaries

| Layer | Responsibility |
| --- | --- |
| `generate/application.rs`, `ui/theme.yaml` | Typed component composition, transport declarations and bounded visual tokens |
| Component/generator primitives | Semantic Source, Region and PaneLink components, safe DOM, focus, geometry and lifecycle |
| `forge-ide-ui` model/projection | Context, report/search/selection, source acceptance, source windows and typed presentation |
| Generated worker and `src/wasm.rs` | Real ABI 3, bounded codecs, declared effects and transport cleanup |
| Native host/agent | Session/Origin/CSRF, configured aliases, mTLS, independent admission and bounded source reads |

Source is a reusable read-only display component, not a JavaScript editor widget.
Region supplies a named visibility group. PaneLink is presentation-only navigation
inside the declared layout. No handwritten application HTML, CSS or JS is introduced.
The default WASM dependency profile still excludes native filesystem/parser/TLS/server
runtimes. Non-IDE component fixtures exercise the same primitives.

## Compatibility and verification

**Local ABI 3 requires an atomic generated asset/WASM deployment.** It adds guarded
source events/effects, source-window bindings, global Find actions and pane reveal
metadata. Older ABI 2 workers/WASM cannot be mixed with this bundle. Source callers
and serialized event producers must use the matching contracts.

The browser/native protocol remains version 1 with a new typed source command on
the existing `POST /forge/v1/dispatch` gateway. Core Task/IoClient, bootstrap/provider
and shared bridge ownership remain intact. This is not a claim of source-level
compatibility for exhaustive Rust enum users or direct view constructors.

```sh
make ui-check   # deterministic assets, actual WASM, worker and presentation checks
make ci         # native/profile/dependency and strict checks plus ui-check
make ui-qa      # separate sandboxed Chrome and real-agent acceptance
```

Pure model, native and Node checks are separate from real-browser acceptance.
Review fresh desktop/narrow screenshots and exercise source reads, Find, keyboard,
line paging, changed-source errors and lifecycle clearing. The suite's real-agent
path and controlled fault probes retain distinct evidence. A screenshot alone does
not establish keyboard, screen-reader or admission correctness.

## Proposed, unimplemented editor-host lifecycle

An actual JavaScript editor can fit the same ownership boundary, but the current
source component is read-only. Editing and saving require separate contracts:

- WASM would own document/base revision, dirty/conflict/save policy and sequenced
  edit acknowledgments. It would issue typed display/update/close requests and
  validate callbacks from the current editor instance.
- A reusable generated main-thread host would mount/update/dispose the widget and
  own DOM, caret, focus, composition and a bounded presentation buffer. It would
  neither fetch source nor decide when a document is saved. Programmatic updates
  must not echo back as new user edits.
- Only WASM could prepare a future write effect. Host and agent would independently
  authorize it and enforce the expected source revision. A widget callback or
  optimistic label cannot prove successful publication.
- Context loss would dispose editor content/listeners, reject late callbacks and
  restore focus appropriately. Hiding a pane alone must not discard unsaved work.
  Pagehide and fatal recovery would clear protected editor content synchronously.

Writes, revision conflicts, editor widgets and edit/composition/save lifecycle
remain unimplemented. They need explicit model/codec, admission and real-browser
verification before being presented as working capabilities.

The [inline-edit proposal](../../../docs/architecture/proposals/forge-ide-inline-edit.md)
designs a contextual prompt, a staged diff, and guarded Apply and Undo actions.
