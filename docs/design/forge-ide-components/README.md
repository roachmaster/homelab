# Forge IDE — components, views and transitions

**Implemented workbench composition, with design vocabulary retained.** This
started as the [wireframe](../forge-ide-wireframes/README.md) decomposition at
[`6e86ad0c`](https://github.com/roachmaster/homelab/tree/6e86ad0c45c3b26fb14d53ab9d640f00e4a88ba1),
2026-11-02. The [runtime workbench guide](../../../src/rust/forge-ide-web/WORKBENCH.md)
now maps the contracts to typed declarations, generic generator primitives and the
worker/WASM model. C01–C09 remain composition names, not public Rust types or crates.
The original wireframe images and earlier PDF remain historical design artifacts;
they are not acceptance screenshots. Browser acceptance is separate from source,
native and Node validation.

Read [component contracts](components.md) for inputs/outputs and ownership, then
[transition contracts](transitions.md) for guards, effects, lifecycle and tests.
Apply [Forge IDE UX](../../../.agents/skills/design-forge-ide-ux/SKILL.md) and
[Forge UI authoring](../../../.agents/skills/design-forge-ui/SKILL.md): YAML tokens
and typed Rust declarations generate the application assets; no handwritten app
HTML/CSS/JS or hidden raw-code templates.

## Three architecture views

| View | Editable source | SVG | Question |
| --- | --- | --- | --- |
| M26 component ownership | [Mermaid](../../architecture/diagrams/mermaid/m26-ui-components.mmd) | [Image](../../architecture/diagrams/mermaid/images/m26-ui-components.svg) | Which component composes which views, and where does application state live? |
| M27 screens and local navigation | [Mermaid](../../architecture/diagrams/mermaid/m27-ui-navigation.mmd) | [Image](../../architecture/diagrams/mermaid/images/m27-ui-navigation.svg) | What is a screen, a panel or a state—not an HTTP route? |
| M28 inspection lifecycle | [Mermaid](../../architecture/diagrams/mermaid/m28-ui-transitions.mmd) | [Image](../../architecture/diagrams/mermaid/images/m28-ui-transitions.svg) | When can a result become visible, and what invalidates it? |

![Component ownership](../../architecture/diagrams/mermaid/images/m26-ui-components.svg)

![Screen and navigation](../../architecture/diagrams/mermaid/images/m27-ui-navigation.svg)

![Guarded inspection lifecycle](../../architecture/diagrams/mermaid/images/m28-ui-transitions.svg)

These complement the actual [M25 runtime architecture](../../architecture/diagrams/mermaid/README.md#m25--generated-ui-implementation),
not replace it. M26–M28 now describe the implemented composition and accepted-report
guards. The tables, not a simplified arrow,
define error and stale-event behavior.

## Pages, panels and states

There are **two screen compositions**, one static application document and no new
backend URLs. “Page” below means a client-side screen composition, not another
server handler or a route per task.

| Composition | Content | Entry / exit |
| --- | --- | --- |
| **Connection** | Session resume status, pairing form, bounded validation/recovery message | Worker startup sends Resume. A matching validated catalog enters Workbench. Failed resume/pair stays here. Not a username/password login or certificate manager. |
| **Workbench** | Context toolbar; Explorer; central inspection workspace; optional Outline; Problems; status/view controls | Requires an accepted catalog. It can have no allowed project or no report yet. Changing context, loading, partial results and failures do not create separate pages. |
| **Worker recovery** | Generated fail-closed message with explicit reload | Not a third domain page. Fatal ABI/startup failure is outside `Phase`; stop dispatch and recover via a new instance. |

Within Workbench:

- **Project overview / no selection** and **file + declaration details** are
  central content projections. A file-local symbol selection is not an editor tab.
- **Explorer, Outline and Problems** are panes. Desktop can show several; narrow
  mode exposes one dominant pane with visible switches. Problems may be expanded
  into the working area without navigating to a new document.
- **Idle, Loading, Ready, Partial, Failed and Cancelled** describe the inspection
  lifecycle. Empty data, no matches, binary content and unavailable Rust syntax
  are content conditions—not additional global phases or pages.
- A complete empty project still has its root entry. “No declarations” is valid
  only for inspectable data with no emitted declarations; unavailable parsing is
  a different message. Do not turn zero results into a compiler/type-check claim.

## Navigation and persistence decision

**This workbench slice keeps one document at `/`; do not add History API entries,
deep links, URL query state or an application Back/Forward stack.** Explorer,
Outline, Problems and pane switches provide local navigation. This deliberately
avoids treating report-local integer IDs as stable cross-refresh addresses.

Browser Back/Forward keeps normal browser behavior and may leave the document.
Existing `pagehide` tears down the worker; a restored page starts a fresh worker
and resumes its host session. Refresh also obtains a fresh admitted catalog,
selects its current default node/project and starts with no report. Neither action
replays an inspection or restores an old report automatically. If resumable
navigation/deep links are later requested, design stable identifiers and privacy
rules separately rather than serializing current view state into URLs.

Generated presentation clears protected DOM during teardown and before session
recheck, including browser-cache restoration. Fatal worker recovery clears content
and closes dispatch; it never keeps using a broken WASM instance.

During a live worker instance, switching panes or focus preserves valid selected
entry/declaration, expansions, filters and pagination **within the same accepted
report**. Geometry, scroll and focus can survive pane closure/reopening in memory.
A new inspection, node/project change, session replacement or teardown invalidates
report-scoped selection and diagnostic references. Do not carry them to a new
report even when it happens to reuse entry `0`. First slice adds no localStorage,
report cache or cross-tab state. Pairing codes, CSRF, session identifiers, reports
and protected aliases must not be copied into URLs/history/storage/logs.

## Implemented module map

| Package | Cohesive implementation |
| --- | --- |
| `forge-ide-ui` | `protocol`, `model`, private `model::projection`, `view`, `validate`; accepted request context on every local intent, bounded name filters/windows and Problems navigation; no native I/O |
| `forge-ide-web` | Typed Screen/Toolbar/Filter/PaneLayout/Pane/Reload plus existing primitives; YAML tokens; generic keyboard/geometry/lifecycle adapters; real WASM ABI 2 |
| `forge-ide-web-host` | Asset verifier now requires bundle ABI 2; pairing/session/CSRF, aliases, client factories/providers and shared bridge behavior stay unchanged; owned test fixtures expand |
| `forge-agent` / inspector | Existing read-only configured-project inspection over mTLS unchanged; no new remote operation |

The implementation stages were composition, keyed projections, reusable pane
primitives and interrupted/repeated browser flows. Their acceptance scenarios
remain in [transitions](transitions.md). Native/model/generator/Node checks and
real-browser desktop/narrow journeys are distinct gates, not interchangeable proof.
Screen-reader certification, editor/LSP, source bodies, arbitrary discovery,
read-file/write-file, Run/Debug and a general docking framework remain out of scope.

**Compatibility:** public UI event/view contracts changed; local WASM ABI is now 2.
Replace generated assets and WASM atomically. The native browser protocol remains 1,
as do existing inspected DTOs and shared ingress/core contracts. See the
[precise migration](../../../src/rust/forge-ide-web/WORKBENCH.md#compatibility-and-verification).
Pure model/components need no empty registrar; native capabilities keep public
config/factory/registrar integration. No new crate per widget or UI service locator
was added, and no deployment or operational credential setup is part of this slice.

## Rebuild the diagrams

Use the [existing pinned Mermaid pipeline](../../architecture/diagrams/mermaid/README.md#rebuild-locally),
which validates official grammar, renders locally and records source/SVG hashes.
Inspect the three diagrams after rendering; no external rendering service receives
repository content. Their SVGs are design documentation, not runtime UI templates.

[Wireframes](../forge-ide-wireframes/README.md) · [Homelab](../../../README.md) ·
[Documentation](../../README.md)
