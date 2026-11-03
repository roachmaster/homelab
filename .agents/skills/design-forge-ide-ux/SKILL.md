---
name: design-forge-ide-ux
description: Design or review Forge IDE workbench UX, wireframes, information hierarchy, keyboard flows and inspection states. Use for an IDE interaction/layout proposal before implementation, not a marketing page or an instruction to build an editor.
---

# Design Forge IDE UX

Make it easy to **choose an authorized project, inspect it, find a file or
declaration, and understand the result's limits**. Design a workbench around this
job, not a dashboard of equal cards or a decorative imitation of a full IDE.

Use [Design Forge UI](../design-forge-ui/SKILL.md) for authoring/execution rules:
application assets come from YAML/typed Rust components; main-thread presentation
stays separate from worker/WASM application state. This skill owns the experience,
not a competing frontend architecture. Its [source notes](references/sources.md)
and [pinned adaptation record](../../../docs/design/skill-sources.md) distinguish
local guidance from upstream material. No upstream skill suite is installed.

For a visual critique, polish or design-direction request, read
[Workbench craft](references/workbench-craft.md): it adapts design-taste and
Impeccable guidance to an operational IDE, not a landing page. For implementation
acceptance, use [Test Forge UI](../test-forge-ui/SKILL.md); wireframes and aesthetic
rules are not usability or browser-test evidence.

## Establish what the screen can truthfully show

Inspect the actual [portable protocol](../../../src/rust/forge-ide-ui/src/protocol.rs),
[view bindings](../../../src/rust/forge-ide-ui/src/view.rs),
[component declarations](../../../src/rust/forge-ide-web/src/generate/application.rs)
and a current screenshot when available. Record **existing data/actions**, **new
presentation**, and **new capability requirements** separately.

- The current product inspects read-only file metadata and Rust declarations. It
  does not return full source bodies, resolve types, edit files or run/debug code.
  Do not invent source text, signatures, compiler diagnostics or an editable tab.
- Node/project choices come from the admitted session catalog, not arbitrary
  addresses/paths. A visible option is not authorization.
- Existing diagnostics may be plain strings. The Problems view must not
  invent compiler severity or line locations. Missing/unreadable/binary analysis
  is not an empty successful file. Derived counts describe inspected data only.
- Label every proposed action and widget. A wireframe demonstrates a design, not
  implemented resizing, search, browser behavior or accessibility conformance.

## Shape a workbench, then test its interactions

1. **Prioritize the journey.** Keep node/project, connection status and Inspect
   together in compact context chrome. Pairing is an entry state, not a permanent
   oversized panel after admission. Give the selected file/declaration most of the
   workspace; place Explorer, optional Outline and Problems around it. Omit
   unrelated Run/Debug/Terminal controls instead of presenting pretend features.
2. **Use readable density.** Align names, kinds and spans; use consistent row and
   spacing tokens. Use familiar original file/symbol/status glyphs with text or
   accessible labels, never color alone. Reserve emphasis for selection, focus,
   actionable failure and the primary action. Long/native-encoded names need a
   discoverable complete representation; truncation alone loses information.
3. **Specify navigation.** Distinguish focused from selected rows. Define tree
   expansion versus selection, a non-pointer route through each pane, and focus
   restoration after collapse, results and errors. Pane resizing needs keyboard
   and reset alternatives. Narrow windows change the pane arrangement rather than
   shrinking every pane into unreadable columns. Avoid browser-shortcut conflicts.
4. **Design adverse states.** Cover unpaired, no allowed project, no report,
   loading, complete, partial, unavailable/denied, timeout and local cancellation.
   Keep accepted content tied to its node/project and request revision. A changed
   selection or late response must not display another project's report. Retry is
   explicit; no fake progress percentage or claim that Cancel stopped the agent.
5. **Keep new tools honest.** The implemented file/symbol filter operates on the bounded
   accepted report unless a separate backend contract is requested. It is not
   full-repository or source-content search. Specify what pagination, exclusions
   and partial data mean for matches. Do not add backend effects just to make a
   pictured control appear functional.

## Wireframe first

For a wireframe request, produce editable design-only SVGs and readable previews,
plus a short interaction/status legend. The SVG is documentation, **not** a raw
runtime asset/template or a loophole for hand-authored application HTML/CSS/JS.
Use fictional data shaped like actual DTOs. Start with the main task, one partial
or failure view and a narrower layout when they clarify real decisions; do not
generate arbitrary visual variants as a substitute for a coherent flow.

Annotate ownership: main thread owns pane/focus/scroll presentation; WASM owns
selected project/file/declaration, filters and request/result policy; worker glue
performs only approved effects; host/agent retain admission. Map any later change
to typed declarations/tokens or a reusable generator primitive. Missing primitives
are implementation gaps—not permission to hide app-specific JS in an emitter.

Use the [first workbench wireframes](../../../docs/design/forge-ide-wireframes/README.md)
as a concrete status-labeled example, not a permanent pixel specification.
For a component/page breakdown, consult the [decomposition](../../../docs/design/forge-ide-components/README.md): separate screen compositions from local panes and asynchronous states, identify typed inputs/events and their owner, and make navigation/history policy explicit without inventing stable report IDs or backend routes.
The [current workbench guide](../../../src/rust/forge-ide-web/WORKBENCH.md) distinguishes
implemented controls from the earlier wireframe notation. Verify actual desktop
and narrow browser behavior when implementing; do not call new layout accepted
from native model tests or static visuals alone. Keep focus distinct from selection,
all report-local actions keyed, and pagination bounded and reversible.

## Review and handoff

- Can a user identify the current project, selected file, selected symbol and
  whether the report is complete without reading a tutorial?
- Can the same task be described with keyboard-only steps? Are selection, focus,
  loading and error visually distinct, with meaningful labels?
- Do long names, safe text, empty/partial reports, repeated clicks, stale replies
  and narrow windows retain context without fake data or automatic retry?
- Does each proposed control identify existing support versus required model,
  generator or backend work? Is the authoring/worker/admission boundary preserved?
- Inspect rendered pixels for clipping/legibility and verify file links. Static
  wireframe review is not a browser, assistive-technology or backend test.

Return **journey; status/data inventory; visuals; interaction/state contract;
implementation gaps; checks; decisions needed before implementation**. When asked
for wireframes first, stop at that design deliverable. Skill invocation grants no
runtime redesign, new capability, deployment or unrelated refactor authority.
