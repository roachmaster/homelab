# Forge IDE — components, views and transitions

**Implementation reference, reviewed 18 November 2026.** The names C01–C09 are design compositions built from real reusable primitives, not exported Rust classes. The current model is in [forge-ide-ui](../../../src/rust/forge-ide-ui/src/model.rs); declarations are in [generate/application.rs](../../../src/rust/forge-ide-web/src/generate/application.rs). The local browser ABI is **5**, HTTP protocol **1**.

## Current screen map

| Surface | Current composition | State and navigation |
| --- | --- | --- |
| Connection | Resume or bounded one-time Pair | Admission precedes protected views. |
| Chat | Compact title/history drawer, New chat, transcript, inline composer and optional model controls | History search/retention live in the drawer. Typed async run state and saved transcripts are distinct. |
| Mail | Inbox/Archive, Compose, thread messages and Reply | Visible only when supplied by the admitted catalog. Status is generation state, not unread persistence. |
| Workbench | Header breadcrumb/sheets, global navigation, pane strip and primary source/file area | Desktop panes and one active narrow pane; verified source, optional managed editor and guarded review. |
| Home/Design | Separate generated documents in the same tab | Save/handoff first; return restores admission, drafts and source bookmark. |
| Fatal recovery | Protected DOM cleared, worker closed, explicit recovery | Adapter lifetime state is distinct from an inspection phase. |

The [component contract](components.md) and [transition contract](transitions.md) describe current behavior. [Current implementation](../implementation-status.md) distinguishes repository support from configured live features. Current phone/desktop captures are published at `/design/wireframes`; the older PDFs remain historical.

## State ownership

Rust/WASM owns application state, request correlation, report/search mode, source acceptance, Chat/Mail semantics and edit revisions. Main JS owns DOM, focus, native dialogs, source wrapping/display choice and browser lifecycle. Worker JS performs the declared I/O and codecs; native host/agent/services enforce their own admission.

Tab-local presentation (pane geometry/focus/scroll) differs from a durable checkpoint. Saved Chat history and queued work live in PostgreSQL. Workbench bookmarks require a new inspection/read. A bounded sessionStorage JSON handoff can recover supported drafts while navigating Home/Design. It excludes credential material and completed transcript bodies; it is not an independent authoritative store or generic offline cache. Mail has a separate navigation snapshot.

## Navigation and compatibility

Ordinary pane/tree selection does not add browser history entries or expose report IDs in URLs. The docs' bounded `#chat`, `#mail`, `#workbench` entry fragments are explicit view intent: they map to existing Rust events after admission/restoration and are consumed. The Forge wordmark opens `/home`; Design opens the docs index. Browser Back can leave the page, subject to browser leave protection.

Source is read-only by default. The actual application also has a bounded textarea editor and managed Apply/Undo/status path where configured. This does not introduce an LSP editor, terminal or arbitrary filesystem URL. Deploy the verified asset bundle atomically; ABI2/ABI3 historical diagrams are not compatible current runtime specifications.

## Evidence

[28-group browser guide](../../../src/rust/forge-ide-web/tests/browser/README.md) · [Workbench](../../../src/rust/forge-ide-web/WORKBENCH.md) · [UI boundary](../../architecture/proposals/forge-ide-ui.md) · [Editable diagram gallery](../../architecture/diagrams/mermaid/README.md).
