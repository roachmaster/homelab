---
name: design-forge-ui
description: "Design, implement or review Forge UI authoring and execution boundaries: declarative generated web assets, main-thread presentation, and worker/WASM application logic. Use for Forge components, styling, UI generation or browser effects; not generic hand-written frontend work."
---

# Design Forge UI

**Application authors do not hand-write HTML, CSS or JavaScript.** This is the
user's Forge authoring constraint, not a universal web-development rule. Read the
[current UI proposal](../../../docs/architecture/proposals/forge-ide-ui.md#5-build-output-is-not-runtime-behavior)
for context. Its older PDF/diagram snapshot does not override this rule. The [initial implementation](../../../src/rust/forge-ide-web/README.md) now supplies
`component!`, strict YAML tokens and generated assets, with a portable Rust model
and native host. Broader widget/style APIs remain unimplemented; inspect the actual
exports. Native/Node tests do not substitute for actual browser acceptance.

For workbench layout, inspection-state UX or wireframe-first design, also use
[Design Forge IDE UX](../design-forge-ide-ux/SKILL.md). It owns interaction and
visual hierarchy; this skill retains the authoring/execution boundary. Design-only
SVG documentation does not become a hand-authored runtime asset.

## Author declarations, generate web assets

- Styles come from **YAML style/theme declarations** or **Rust UI-component macro
  declarations**. Prefer validated properties/tokens and typed component attributes.
- Generate simple semantic HTML from the component model: stable mount regions,
  accessible controls and text slots. Do not author application HTML pages/templates.
- Generate minimal JS glue for messages, browser APIs, DOM presentation and worker
  lifecycle. Application computation and business decisions belong in Rust/WASM,
  not hand-written listeners, fetch handlers or JS state reducers.
- Hiding CSS/HTML/JS in Rust strings, `.forge` blobs, YAML raw-code fields or macro
  string arguments is **not** declarative authoring. Do not use that escape hatch.
  Low-level generator emitters may contain audited reusable primitives, owned and
  tested as generator implementation—not a place to stash each application's code.
- Keep declarations, generator implementation and generated output visibly separate.
  Missing generator support is a scoped implementation/design gap, not permission
  to hand-write the output or invent an entire macro framework without a request.
  Do not delete/migrate existing assets merely because this skill is invoked.

## Divide state and execution deliberately

```text
main thread: display + browser lifecycle / presentation state
  → typed application event → worker: WASM model / computation
  → typed prepared effect → generated worker I/O glue → admitted backend gateway
  → typed result → WASM acceptance / view projection → main-thread display
```

| Owner | Owns | Must not become |
| --- | --- | --- |
| Main thread | DOM, focus, accessibility, scroll, mount/unmount, worker startup/teardown and local presentation/projection state | Backend fetch route, business computation or a second authoritative domain/request-state store |
| Rust/WASM in worker | Application intent, domain/request semantics, selection/revision policy, computations, effect preparation, stale-result acceptance and typed views | Native filesystem/TLS code imported into the browser or untyped JSON state |
| Generated worker glue | Bounded event/result codecs, WASM ABI/buffer plumbing, async network I/O from approved WASM effects, abort/cleanup | A JS business controller, service locator or arbitrary URL proxy |
| Native host / remote agent | Session admission, retained credentials, configured destinations, independent operation/project authorization and typed shared-bridge dispatch | Trust in browser identity headers or unrestricted relay |

The “WASM backend” is local to the worker; it is not the remote Forge agent.
Forward **application-relevant inputs and results** through WASM as typed messages.
That does not mean shipping every mousemove, DOM object, credential or unrelated
page data to WASM or the remote server. Browser-required local presentation work
remains on the main thread. Application data reaches a remote backend only when
WASM emits the corresponding permitted effect. Main-thread asset loading/worker
startup is not a business API call.

## Review a proposed change

1. **Locate its authored source.** Identify YAML/component declarations, generator
   ownership and outputs. Verify macro/schema/API availability; mark illustrative
   names proposed. No editable application HTML/CSS/JS source may hide in templates.
2. **Trace the interaction.** Name input DTO → WASM event → typed effect → worker
   client → host/agent gate → result event → accepted view. Keep native `Task` and
   `IoClient` boundaries; an async JS promise is not the current synchronous Rust
   IoClient implementation. Pure model logic needs no empty registrar or ABI DI container.
3. **Assign state and lifetime.** Name main-thread presentation state versus WASM
   domain/request state. Bound messages, reports and buffers; preserve session epoch,
   revision, request ID and deadline. Ignore stale replies. Cancel/unmount stops local
   ownership and fetch where possible, not proof of remote task cancellation.
4. **Preserve admission.** Use the [front-door skill](../design-forge-front-door/SKILL.md)
   and [command trace](../trace-forge-command/SKILL.md): one command path,
   header-selected typed destination/variant agreement, no main-thread bypass.
   Session-filtered selectors are not authority. Keys stay native; host and agent
   permissions remain independent. Render source-derived text safely, never as HTML.
5. **Prove the generated contract.** During authorized implementation, test schema
   rejection, deterministic generation/drift, no overwritten authored files, real
   WASM ABI compatibility, dependency omission and actual browser interactions.
   Cover repeated clicks, stale replies, partial/error views, focus/accessibility,
   mount/unmount, bounds and zero backend calls on rejected admission. Export real
   public config/factory/registrar integration where a reusable native capability
   is delivered; use [capability delivery](../expose-forge-capability/SKILL.md).

## Catch these shortcuts

| Proposed shortcut | Correct direction |
| --- | --- |
| Add `app.css` and an inline click/fetch handler | Declare component styles/actions; generate the CSS and message glue |
| Move that handler into a Rust raw string or YAML `script` field | Keep application semantics in Rust; extend an audited generator primitive only when needed |
| Main thread computes the tree summary or calls `/inspect` | Worker/WASM computes; worker effect uses the configured single command gateway |
| Worker JS chooses the newest response and rewrites domain state | WASM validates the typed result/token; main thread applies only accepted views |
| “Everything in WASM” removes DOM focus/lifecycle handling | Keep browser presentation/lifecycle on the main thread; distinguish it from domain authority |

Return **authored declarations → generated artifacts; state/worker ownership;
request/admission trace; existing versus proposed APIs; checks and gaps**. For a
docs-only change, validate links/scenarios and report that no browser runtime was
built or tested. This skill adds no deployment, installation or unrelated rewrite
authority.
