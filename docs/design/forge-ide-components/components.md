# Component contracts and ownership

Part of the [workbench architecture](README.md). **C01–C09 name implemented
compositions, not new crates or exact public Rust exports.** The catalog below
states the component contracts; concrete primitives and serialized events are in
the [workbench guide](../../../src/rust/forge-ide-web/WORKBENCH.md) and linked source.

## Composition tree

```text
C01 AppShell
├─ C02 ConnectionView                  when no admitted catalog
└─ Workbench composition               after admitted catalog
   ├─ C03 ContextToolbar
   ├─ C08 ViewControls / SplitPane / narrow-pane switch
   ├─ C04 ExplorerPane
   ├─ C05 InspectionWorkspace          selected file/declaration details
   ├─ C06 OutlinePane                  optional supporting navigation
   ├─ C07 ProblemsPane                 optional accepted-report diagnostics
   └─ C09 StatusRegion
```

The shell selects compositions from accepted model state. A hidden pane is not a
new worker, request owner or replica of the report. All panes share one typed model
projection, and display-only controls cannot choose remote destinations.

## Catalog

| Component | Typed inputs / properties | Input events → output | State owner and existing support |
| --- | --- | --- | --- |
| **C01 AppShell** | Screen kind, projected view, worker availability; declarative layout | Mount/page lifecycle → generated worker startup/teardown; accepted views → semantic regions | Main thread owns browser lifetime/focus, not session authority. Typed Document/Screen/Panel and PaneLayout compose the screens. |
| **C02 ConnectionView** | Resume/pair pending status, bounded failure text, field validation | Submit pairing value → existing `Pair`; startup → existing `Resume`; explicit reload → new worker | WASM owns request status; generated field clears secret after submission. Existing PairForm/Select/Text/Button cover most composition. No certificate upload or remote host discovery. |
| **C03 ContextToolbar** | Filtered `Catalog` choices, selected node/project, phase, available actions | `SelectNode`, `SelectProject`, `Inspect`, `Cancel` → typed model dispatch | Existing events/effects; compact arrangement implemented. Values are configured keys/aliases, never arbitrary URLs/paths. A busy action is disabled and guarded in model. |
| **C04 ExplorerPane** | Report key, bounded ordered entry rows, selected ID, expansions, bounded query/match counts | Keyed selection/toggle/previous/next and `SetFileFilter` → local projection | WASM owns meaning; main owns focus/scroll. Tree/ListBinding/RowView plus FilterBinding, glyphs and complete-name presentation. |
| **C05 InspectionWorkspace** | Accepted report summary, optional selected entry/declaration, safe metadata/diagnostics | Select a projected declaration → report-scoped local selection; no effect | Main presents; WASM chooses current entry/details. Existing Text/Details/TableBinding can express much of it. The central declaration hierarchy uses the same reusable Tree primitive. No source body/signature editor. |
| **C06 OutlinePane** | Report key + entry ID, file-local declaration rows, selected declaration, optional query | Keyed select/previous/next and `SetSymbolFilter` → local projection | WASM owns hierarchy/selection/filter; main owns focus. Existing Tree, with new kind-glyph and filter declarations. Symbol IDs are local to a file and report. |
| **C07 ProblemsPane** | Report key, bounded rows `{entry_id, diagnostic_index, text}`, visible count/completeness | `SelectProblem` → select associated entry, clear symbol selection and reveal supplied diagnostic; no remote effect | WASM projection over existing strings; main presentation. No invented severity/source span or compiler result. Empty Problems does not make an incomplete report complete. |
| **C08 ViewControls** | Declarative pane IDs, min/max/default geometry, visible panes, narrow mode | Resize/collapse/reset and narrow-pane switch → presentation only | Main-thread generic primitive owns geometry/active pane/focus restoration; it cannot filter or alter application selection. Typed PaneLayout/Pane declarations, not app-specific JS. |
| **C09 StatusRegion** | Phase, selected context, completeness and safe actionable failure category | Announce meaningful changes; explicit Retry maps to existing Inspect; Reload maps to lifecycle recovery | WASM supplies meaning; main presents and announces. Existing Text.live/Button; no fake progress percentage, machine-health or cancellation guarantees. |

## Local data contracts and design notation

The following keeps the original **illustrative design notation**, not exported
wrapper types. Actual `UiEvent`/`RowAction` variants carry `report: RequestToken`
directly; outline/symbol variants also carry `entry`. Both pagination directions
and Problems use the same guard:

```rust
// RequestToken is retained as accepted report context in UiModel.
struct ReportKey(RequestToken);
struct EntryRef { report: ReportKey, entry: u32 }
struct SymbolRef { entry: EntryRef, declaration: u32 }
struct ProblemRef { entry: EntryRef, diagnostic_index: u16 }

enum LocalIntent {
    SelectEntry(EntryRef),
    SelectSymbol(SymbolRef),
    SelectProblem(ProblemRef),
    ToggleEntry(EntryRef),
    MoreTree { report: ReportKey },
    MoreOutline { entry: EntryRef },
    SetFileFilter { report: ReportKey, query: BoundedQuery },
    SetSymbolFilter { entry: EntryRef, query: BoundedQuery },
}
// Geometry events belong to generated presentation; no HTTP effect is possible.
enum PaneIntent { Show(PaneId), Hide(PaneId), Resize(PaneId, BoundedSize), Reset }
```

All local events require matching **accepted** `ReportKey` plus valid IDs
before altering selection, expansion, filters or pagination. The guard applies
to **all report-scoped intents**, not only the three Select variants. Outline
pagination/filtering also checks that its EntryRef is still the selected entry.
The old integer-only action contract was replaced in local ABI 2. Network results
already checked the full token; accepted-report retention now protects row actions
too. `ReportKey` is internal
view context, not authentication or a stable link. Opaque IDs and validated
strings remain typed values; do not introduce `Any` or untyped JSON app state.

Implemented filter rule (the design calls it `BoundedQuery`): at most **256 UTF-8 bytes**, case-sensitive
literal substring against the same safe display name shown to the user; empty
means no filter. No regex, query language, network search or implicit path access.
This is the current bounded behavior, not a universal search language.
Match across the entire accepted bounded report before paginating visible rows;
retain parent context. A selected entry hidden by the filter remains the selected
workspace item with a clear “hidden by filter” indication until explicitly changed.
Clear the query on new report/context; changing files clears the symbol query.

Problems indexes refer to existing diagnostic strings; retain references or copy
within existing message/text limits, rather than duplicating unbounded text.
Page displayed rows (up to 256 plus an encoded-byte budget per list); never add a
backend call to reveal more accepted local rows. Existing validation caps report
entries at 4096, declarations at 16384 total, diagnostic strings at 16 per entry,
and aggregate text at 2 MiB. Every projection must respect the 8 MiB message cap
including its own overhead and communicate truncation/pagination truthfully.

## Reuse primitives without losing semantics

Existing [component declarations](../../../src/rust/forge-ide-web/src/generate/component.rs)
include Screen, Panel, Toolbar, Row, Workspace, PaneLayout/Pane, Text, Select,
Button, Filter, Reload, PairForm, Tree and Details.
Existing [bindings](../../../src/rust/forge-ide-ui/src/view.rs) include text,
choices, buttons, filters, lists, tables and visibility. Build compositions with these
first. Add the smallest typed generator primitive when these cannot express a
required interaction: input/filter control, separator, pane switch, glyph badge,
or diagnostic list—not a bespoke raw HTML/JS block per component.

Semantic names, visible labels, hover/full-name help and keyboard behavior are
part of a primitive's contract. Prefer a selected-item details region for the
complete name so it is not hover-only. Treat diagnostic/type/name text as text,
including `<script>`-like and native encoded values. Icon meanings need labels;
selection, focus, partial state and disabled controls must remain distinguishable.

## Ownership checks

- **Main:** mount/unmount, DOM, accessible focus, scroll, resizer geometry and pane
  visibility. A narrow pane switch preserves application selection; a focused
  item hidden by collapse moves focus to a visible reopen control.
- **WASM:** catalog-based choices, report ownership, entry/symbol/problem selection,
  filtering, request revisions and result acceptance. No native path/TLS imports.
- **Worker adapter:** bounded codecs/ABI, approved effects, network and abort
  plumbing. No business-state reducer or arbitrary URL relay.
- **Native host/agent:** independent session and operation/project admission,
  shared header/variant gate and existing inspection task/client boundaries.

No component is an authorization boundary by itself. UI availability mirrors
accepted information; every backend invocation is still checked at destination.
See [transitions](transitions.md) for stale events, session recovery and focus rules.
