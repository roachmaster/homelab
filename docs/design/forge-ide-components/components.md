# Current Forge IDE component contract

Reviewed 18 November 2026. C01–C09 identify compositions, not required crates or Rust exports. Structure comes from typed Rust declarations; styling comes from YAML/property maps. Inspect actual component variants in [generate/component.rs](../../../src/rust/forge-ide-web/src/generate/component.rs).

| Composition | Current responsibility | Implementation and guard |
| --- | --- | --- |
| C01 App shell | Forge→Home identity, context/header actions and Chat/Mail/Workbench/Home/Design navigation | BrandLink, HeaderActions, Navigation, Screen. Mail is catalog-gated; Home/Design use the checkpoint handoff. |
| C02 Connection | Resume, one-time pairing and safe recovery | Protected regions remain cleared until accepted session/catalog. Agent credentials stay native. |
| C03 Context | Node/project breadcrumb and a native Project options sheet | SheetTrigger and configured Select controls. Context changes invalidate report/source. No remote-path input. |
| C04 Explorer | Dense tree, safe names, focused-row readout, bounded paging and one search field | Filter mode switches between case-insensitive file/symbol search and literal entry-name filtering. Rust guards report and mode; local edits create no HTTP effect. |
| C05 File workspace | Verified source, highlighted declaration/window, Raw/Display for Markdown, Wrap, contextual prompt and optional editor/review | Source and structured diff/editor primitives. Inspection digest and native revision have distinct roles. Write controls require admitted managed capabilities. |
| C06 Outline | Selected-file declaration hierarchy, filtering, paging and source navigation | Report+entry guards. Syntax declarations are not resolved references or compiler output. |
| C07 Problems | Supplied inspection diagnostics, counts and guarded file cross-navigation | Partial/unavailable states remain explicit; no invented severity, compiler success or line mapping. |
| C08 View controls | Native View options sheet, desktop visibility/resize and narrow pane switches | Presentation state owns geometry. Narrow layout hides redundant pane headings; Escape closes a sheet and returns focus. |
| C09 Status | Current actionable connection/request/error/partial state; supporting inspection details | Idle guidance is disclosed. Late events cannot reopen stale source or steal focus. |

## Chat and Mail

Chat's title trigger opens Conversations, including saved history search, selection and Manage retention actions. User and assistant messages have different presentation; the input label remains accessible without a separate visible row. The inline Send icon retains its name, disabled state and touch target. Default expiry is assigned by storage, and Keep long term/Delete use guarded backend operations.

Mail uses full-width inbox rows, model identity, supplied timestamp/preview and a named Reply ready indicator. Compose is a floating action in the list; the open reader has Back/Refresh and a docked Reply trigger. Recipients use native disclosures and bodies use safe Markdown. Counts, timestamps and previews are not invented when absent. Manual archive/delete and durable unread state are not implemented.

## Editing and source behavior

File source is verified against the inspected digest, bounded to one accepted document and projected into local line windows. Selecting a symbol may request its source once; local line paging does not reread the file. Raw/Display and Wrap are reusable main-thread presentation features over accepted data.

Direct Edit file is a bounded textarea workflow. The Rust model owns editor instance, base identity and dirty draft. Save, Apply, Undo and Check status use the native managed-write boundary and operation journal. Their availability is configuration-dependent. The [inline-edit design](../../architecture/proposals/forge-ide-inline-edit.md) records limits and reconciliation.

## Accessibility and verification

Native keyboard targets, safe text, separate focus/selection, 44px narrow controls, local table/diagram scrolling and visible focus are part of the component contract. Real browser tests cover these interactions with owned fixtures; a diagram or unit test does not establish screen-reader certification or physical Safari behavior. See [transitions](transitions.md) for event and effect ownership.
