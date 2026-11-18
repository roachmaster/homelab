# Forge IDE layouts and wireframes

**Current implementation reference — 18 November 2026.** The live Design reader at `/design/wireframes` publishes current dark-mode generated UI captures for Chat, Conversations, Workbench, project options and Mail. They come from disposable acceptance fixtures, not the operator's private browser or database. [Capture provenance](capture-manifest.json) records their source revision, dimensions and hashes.

## Current layout contract

| Surface | Implemented layout | Important condition |
| --- | --- | --- |
| Chat | Compact Forge/thread-title/New chat header; navigation strip; open transcript; inline composer | Conversations and retention management are in a drawer. Model controls are progressive. |
| Conversations | Search, saved-thread selection and management in one native drawer | Search scope is the loaded history page. Long-term retention and deletion require backend confirmation. |
| Workbench phone | Header node/project breadcrumb and View options sheet; global tabs plus pane tabs; one primary pane | Unified search switches mode inline. File has verified numbered source, Wrap and Markdown Raw/Display. Pane titles are not repeated in a secondary bar. |
| Workbench desktop | Explorer and primary File area with optional Outline/Problems and resizable panes | Source/diff/editor controls depend on accepted context and native capabilities. |
| Mail | Divider-based inbox, supplied sender/date/snippet, Compose; focused message reader and Reply | Reply ready is generation status. Archive is the recall-limit folder, not a manual mutation. |
| Home/Design | Launchers on Home; compact docs index and separate readers with sticky topics | Same-tab navigation preserves the supported checkpoint/handoff and reauthorizes on return. |

The capture set includes deliberate partial reports and long strings to show real bounds. Editable fields are masked by the acceptance harness. Mail tabs are catalog-gated and are absent in fixtures that disable Mail. A screenshot demonstrates a rendered state, not backend availability, physical Safari behavior or write authorization.

The live portal currently has no Studio relay and its configured source project is read-only. The repository separately implements managed edits, a bounded textarea editor and a static-web Studio service. See [current implementation](../implementation-status.md), [component contract](../forge-ide-components/README.md) and [Workbench](../../../src/rust/forge-ide-web/WORKBENCH.md).

## Current images

The published assets are embedded from [the host's design asset directory](../../../src/rust/forge-ide-web-host/design):

- `forge-ide-chat-phone.png`, `forge-ide-chat-desktop.png`, `forge-ide-chat-history-phone.png`.
- `forge-ide-workbench-phone.png`, `forge-ide-workbench-explorer-phone.png`, `forge-ide-workbench-project-options.png`, `forge-ide-workbench-desktop.png`.
- `forge-ide-mail-inbox-phone.png`, `forge-ide-mail-thread-phone.png`, `forge-ide-mail-thread-desktop.png`.

Regenerate the real UI through the existing `make ui-qa` workflow, inspect only the owned fixture captures, and export the selected files with updated provenance. Do not restyle screenshots or hand-author app HTML/CSS/JS to make a documentation image. Keep capture filenames/route dimensions synchronized with the typed reader and native asset allowlist. New screenshot states require the harness and CI's explicit safe artifact allowlists.

## Archived early schematics

The three `forge-ide-workbench-{desktop,partial,narrow}.svg` files, their original PNG previews and `build_wireframes.py` preserve the earlier structural-inspection proposal. Their proposed filters, old context toolbar and absent source body describe that earlier milestone. They are historical schematics, not current screen designs or a list of missing implementation.

The separate inline-edit schematic is also an earlier design reference; current editing behavior is defined by the implemented revision/fence/journal contracts. Rebuilding those historical schematics reproduces history and does not update the current generated application.

[Documentation index](../../README.md) · [UI architecture](../../architecture/proposals/forge-ide-ui.md) · [Current transitions](../forge-ide-components/transitions.md)
