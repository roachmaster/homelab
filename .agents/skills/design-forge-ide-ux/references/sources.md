# Sources and provenance

Reviewed 2026-11-02. This is an **original Forge-specific synthesis**: no upstream
skill body, implementation, icon set, screenshot or template is vendored. Source
licenses below identify the inspected material; they do not relicense this
repository. A later copy/adaptation must preserve its actual license and notices.
The separate Vercel `agent-skills` wrapper was not imported: its license was not
established in this review. Nothing is installed globally.

## External guidance used narrowly

| Primary reference | Verified source / license | What informs this skill; what does not |
| --- | --- | --- |
| Anthropic frontend design | [Pinned skill](https://github.com/anthropics/skills/blob/ca45fc496a3436251f191354091d557da31c15b5/skills/frontend-design/SKILL.md), [Apache-2.0 license](https://github.com/anthropics/skills/blob/ca45fc496a3436251f191354091d557da31c15b5/skills/frontend-design/LICENSE.txt) | Ground visual choices in the actual subject, use deliberate hierarchy and critique the design before building. Forge's subject is an inspection workbench; marketing hero treatments and hand-written web code are not adopted. |
| Vercel Web Interface Guidelines | [Pinned rules](https://github.com/vercel-labs/web-interface-guidelines/blob/434b7f91364665f2f733b310ec54809bf8f37937/command.md), [MIT license](https://github.com/vercel-labs/web-interface-guidelines/blob/434b7f91364665f2f733b310ec54809bf8f37937/LICENSE) | Review semantic controls, visible focus, asynchronous feedback, long/empty content and alternatives to drag-only actions. Framework-specific recipes, automatic URL synchronization and arbitrary list thresholds are not Forge requirements. |
| JetBrains tool windows | [Official platform guide](https://plugins.jetbrains.com/docs/intellij/tool-windows.html) | Secondary tools can occupy side/bottom regions while preserving the main work area. This is a layout precedent, not a dependency on IntelliJ APIs, a copied product skin, or a requirement to expose Run/Debug. |
| W3C tree view pattern | [Official APG](https://www.w3.org/WAI/ARIA/apg/patterns/treeview/) | Define hierarchy, expansion, focus and selection separately; plan explicit keyboard behavior. ARIA labels alone do not prove a functioning accessible tree. |
| W3C window splitter pattern | [Official APG](https://www.w3.org/WAI/ARIA/apg/patterns/windowsplitter/) | A proposed resizer needs a name, bounded value and keyboard alternative. The page notes its pattern review/example remains incomplete; treat it as design guidance and validate the eventual implementation, not certification. |

Links to living guidance are dated observations, not immutable snapshots. The two
upstream skill/rules links use fixed public revisions. No endorsement or full
compliance with an upstream product is claimed.

## Forge evidence and local decisions

The initial wireframes are grounded in public homelab
[`2913393e24556313ce0f63d86b65022ec6d2aa21`](https://github.com/roachmaster/homelab/tree/2913393e24556313ce0f63d86b65022ec6d2aa21):

- [Portable report/event types](../../../../src/rust/forge-ide-ui/src/protocol.rs):
  real file/declaration metadata, native-name variants and bounded request tokens.
- [Existing authored component document](../../../../src/rust/forge-ide-web/src/generate/application.rs):
  current selection, Inspect/Cancel, trees and details—not a source editor.
- [UI authoring skill](../../design-forge-ui/SKILL.md): user-required YAML/Rust
  declarations, generated assets and worker/WASM ownership.

The compact toolbar, dominant structural workspace, proposed optional panes,
local report filter and Problems aggregation are **our design choices** for the
user's task. They are not existing API claims or instructions from those sources.
Reinspect current code when using this skill after the baseline changes.
