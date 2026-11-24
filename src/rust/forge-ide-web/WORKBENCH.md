# Forge IDE workbench

The generated workbench lets an admitted user find a file or Rust declaration and
read its actual source. Application structure and styles are typed Rust components
and validated YAML; the compiled Rust/WASM model owns project, selection, source
and request policy. Source inspection is read-only by default. Opted-in managed
projects can also save and undo direct edits. The last qualified live `95651a5`
`homelab` project was read-only; source support does not enable writes by itself.

## Use the workbench

[Build and start the host](README.md#run-from-repository-root), then use its configured
entry method. A local host can require its one-use pairing code; a trusted proxy
host uses the proxy's existing sign-in. Agent mTLS keys never enter the page.

1. A catalog with exactly one allowed project opens it automatically after admission.
   Otherwise use the node/project breadcrumb in the header to open **Project options**,
   choose **Node** and **Project**, then use the **Refresh project** symbol beside
   **Problems**. The adjacent header menu opens **View options** in its own native sheet. Changing context clears
   previous report and source ownership. Refresh replaces the loaded report with
   a new inspection. **Inspection details** shows the report status and summary.
2. **Search files or symbols** searches file paths and declaration names in the loaded
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
   Verified Markdown files open in **Raw**. Choose **Display** for a formatted
   preview. Files that exceed the preview limits remain readable in **Raw**.
5. On phones, selecting a file or declaration reveals the File pane. The
   **Back to files** arrow beside **File** returns to Explorer. Pane switches
   still expose Outline and Problems. Selection survives pane changes.
6. Metadata, source-shape inventory and inspection details remain supporting
   disclosures. Problems shows supplied inspection strings, without invented
   compiler severity, test success or source locations.

Explorer uses one query input. Its inline mode toggle switches between case-insensitive
file/symbol search and literal, case-sensitive entry-name filtering; the query survives
a mode switch. The action carries the accepted report and search mode, so stale
mode/report edits are ignored. Outline retains its file-local literal filter. Query
inputs are limited to 256 UTF-8 bytes. No regular-expression evaluation is exposed.

## Source and request ownership

Chat questions, **Send with file**, model-generated proposals and their Apply
controls are inactive. The current portal does not send inference requests from
Workbench. Historical inline-proposal designs are not active controls.

For an explicitly provisioned managed project, **Edit file** and **Save file**
use the direct-edit contract below. A confirmed save refreshes source; Undo
checks the current revision before restoring the saved bytes. An ambiguous
result offers **Check status** for the same operation ID. The agent's journal
survives its restart, while the host's operation index is in memory and browser
recovery depends on the admitted session. Read permission never grants writes.

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
clears the filter draft. After navigation, an admitted tab can recover its
selected project, file and declaration from an admitted tab checkpoint. Forge
obtains a fresh report and verifies source again. Browser session storage holds
the random tab identifier and a bounded navigation handoff containing supported
drafts and bookmarks. Declared `#agents` and `#workbench` fragments select a view
after restore; legacy `#chat`/`#mail` resolve to Agents without retired I/O. Entry
actions are then consumed with `history.replaceState`. Source,
report identities, and credentials do not enter URLs; there is no persistent
report cache, arbitrary source deep link, or cross-tab draft synchronization.

## Authoring and runtime boundaries

| Layer | Responsibility |
| --- | --- |
| `generate/application.rs`, `ui/theme.yaml` | Typed component composition, transport declarations and bounded visual tokens |
| Component/generator primitives | Semantic Source, Region and PaneLink components, safe DOM, focus, geometry and lifecycle |
| `forge-ide-ui` model/projection | Context, report/search/selection, source acceptance, source windows and typed presentation |
| Generated worker and `src/wasm.rs` | Real ABI15, bounded codecs, declared effects and transport cleanup |
| Native host/agent | Session/Origin/CSRF, configured aliases, mTLS, independent admission and bounded source reads |

Source is a reusable read-only display component, not a JavaScript editor widget.
Region supplies a named visibility group. PaneLink is presentation-only navigation
inside the declared layout. No handwritten application HTML, CSS or JS is introduced.
The default WASM dependency profile still excludes native filesystem/parser/TLS/server
runtimes. Non-IDE component fixtures exercise the same primitives.

## Compatibility and verification

**ABI15 requires an atomic matching host/generated asset/WASM deployment.** It
includes guarded source/direct-edit events, source windows, Agents workflows,
event-stream stage diagrams, pipeline stage diagrams, unified search and pane reveal metadata. Legacy Chat/Mail snapshots remain inert;
their retired controls/effects cannot run. Older workers/WASM, including ABI12,
cannot be mixed with this bundle. Source callers and serialized event producers
must use matching contracts. The separately dated
[last qualified deployment](../../../docs/design/implementation-status.md)
does not imply that later repository fixes are already live.

The browser/native protocol remains version1 with the typed source command on
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

## Direct file editing

**Edit file** opens a verified, nonempty text file up to 4 KiB with consistent LF
or CRLF line endings. The main thread presents the textarea, caret and composition
events. WASM owns the draft and identifies the editor instance. It blocks a dirty
file change until the user saves or discards the draft. Switching between
Workbench and Agents keeps the draft open.

**Save file** submits the current inspected revision through `ide.edit_write`.
The host verifies the source and the write agent applies the change using its
revision fence and durable operation journal. Saves preserve the file's newline
convention. A successful save refreshes the verified source and offers Undo in
a managed workspace. Conflicts keep the draft open for review or copying. An
unknown outcome offers **Check status** for the existing operation.

The [earlier inline-edit design](../../../docs/architecture/proposals/forge-ide-inline-edit.md)
records the historical contextual prompt and model-generated diff. Those controls
are inactive; current direct editing is documented above and in the
[Rust editor model](../forge-ide-ui/src/model/edit.rs).
