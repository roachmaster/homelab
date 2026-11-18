# Forge portal landing and documentation navigation

**Implemented reference — reviewed 18 November 2026.** `/home` is the tool launcher and `/design` is the documentation index. The root `/` remains the generated IDE. The original planning baseline was `c57ef4b`; the current composition includes the later Home, Chat, Mail, Workbench and docs-reader updates.

## Routes and roles

| Route | Current responsibility |
| --- | --- |
| `/` | Chat, catalog-gated Mail and Workbench in one admitted Rust/WASM application. |
| `/home` | Solid Open Forge IDE action, adjacent Design action, three icon launchers and compact documentation links. The Forge wordmark returns here. |
| `/design` | Compact topic index: Foundations, Chat and Mail, Game development, Wireframes and workspace. It does not embed full specs or interactive galleries below the directory. |
| `/design/implementation` | Current implemented/configured/remaining matrix with source evidence. |
| Focused design readers | Shared Forge navigation, sticky topics, Design breadcrumb, readable specs and staged previews. |
| `/studio` | Generated static-web build client; requires an explicitly configured Studio relay. |
| `/playtest` | Room One runtime, cooked level/atlas loading, debug/trace and replay controls. |

## Home composition

The hero has one solid primary IDE action beside the quieter Design action. Tool launchers are whole-card links with a leading line icon, a title, one-line task description and a trailing arrow. Documentation rows are lighter and denser. The footer carries repository metadata. The static Homelab workspace label does not claim backend health.

Home uses scoped Rust style declarations, typed nodes and the shared native shell. Its HTML is script-free. Chat/Mail/Workbench remain application screens rather than duplicate launch pages. Current docs view-entry links deliberately support `/#chat`, `/#mail`, `/#workbench` through bounded declared actions; the initial no-deep-link design is superseded.

## Same-tab handoff

1. The generated application link waits for the latest acknowledged workspace checkpoint and a bounded local fallback.
2. Browser presentation navigates to Home/Design in the same tab. The docs themselves do not call the command gateway.
3. Returning starts a new owned worker, resumes native admission and restores supported snapshots. Workbench source is inspected/read again.
4. A declared target view runs after restore. Its fragment is consumed, so a later browser Back does not reapply obsolete view intent.

Credentials and report IDs are not route state. Chat history, Chat tab drafts, Mail navigation drafts and presentation geometry have different persistence scopes; see the [workspace contract](../../../src/rust/forge-ide-web/README.md) and [current implementation](../../design/implementation-status.md).

## Availability and evidence

The checked live host has no Studio relay, although the static-web backend exists in the repository. Its source project has not opted into managed writes. Launchers remain links, not readiness indicators. Broader game export, arbitrary build scripts and a unified Rust/WASM Studio controller remain outside the current configured portal.

[Home source](../../../src/rust/forge-ide-web-host/src/design/home.rs), [docs shell](../../../src/rust/forge-ide-web-host/src/design/mod.rs), [fixed asset routes](../../../src/rust/forge-ide-web-host/src/assets.rs), [navigation declarations](../../../src/rust/forge-ide-web/src/generate/application.rs) and the [browser acceptance guide](../../../src/rust/forge-ide-web/tests/browser/README.md) define the implemented behavior. Native route/CSP tests and same-tab browser journeys remain distinct evidence.
