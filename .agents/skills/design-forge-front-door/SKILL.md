---
name: design-forge-front-door
description: Explain, design or review Forge's single front door with header-selected task dispatch across thin input adapters. Use when adding an entry adapter, HTTP method or destination, or reviewing endpoint proliferation and admission bypass; not for resource-per-task REST design.
---

# Design the Forge front door

**One front door, many internal destinations.** This is the repository's confirmed
application architecture, not a claim that every hexagonal system works this way.
Use the [shared DSL vocabulary](../../../docs/architecture/framework-dsl.md);
inspect actual code before labeling a proposal implemented. This skill adds no
installation, deployment or unrelated refactoring authority.

## Keep the invariant

```text
selected input adapter → canonical Header + typed payload → shared bridge
  → registered destination/version + admission + variant agreement
  → binding → task computation / orchestration → injected clients
```

All supported HTTP operations use **`/forge/v1/dispatch`**, not a URL/controller per
task. The HTTP method supplies operation semantics, the header selects the
registered destination, and the payload supplies its typed arguments. Validate
that method, destination/version and payload agree before business effects. Never
route by inspecting the body, repair a conflicting header, invoke a handler from
an adapter, or interpret a destination as an executable/class name.

CLI, HTTP and any future gRPC or Bitwave adapter must converge on the same logical
internal entry contract and admission rules. This does not promise identical wire
formats, one physical process/thread, one shared object across all applications,
or that these future adapters exist. Internal fan-out belongs to admitted use cases;
ordinary outbound client calls do not need to loop back through ingress.

## Start from the implemented slice

Read the [HTTP projection and limits](../../../src/rust/forge-remote-https/README.md#http-header-projection)
and verify [wire conversion](../../../src/rust/forge-remote-https/src/wire.rs),
[HTTP entry](../../../src/rust/forge-remote-https/src/transport.rs),
[destination policy](../../../src/rust/forge-remote/src/lib.rs) and
[bridge dispatch](../../../src/rust/forge-bridge/src/lib.rs) for the requested change.

- **Current:** loopback-only mutual-TLS demonstration; `POST /forge/v1/dispatch`
  invokes only `node.info` contract version `1`. The body is `{"payload":"info"}`.
  Other verbs return **405 + `Allow: POST`**; other paths return **404**. This is
  an HTTP command gateway (RPC-style), not resource-oriented REST or a deployed daemon.
- Actual routing fields are `Forge-Envelope-Version`, `Forge-Destination`,
  `Forge-Contract-Version`, `Forge-Target-Node`, `Forge-Target-Incarnation`,
  `Forge-Request-Id`, `Forge-Expires-Unix-Ms`, and optional `Forge-Correlation-Id`.
  Read the linked table for exact bounds. Required fields occur once, correlation
  at most once; reject duplicate/case-variant, comma-combined, malformed or oversized
  values rather than choosing a winner. Check envelope version against the path.
- Projection produces the canonical `Header` inside `Envelope<NodeCommand>`;
  target/incarnation and expiry belong to the surrounding `NodeCall`, **not** new
  generic Header fields. Strict body decoding cannot override any of that metadata.
- **Future, not implemented:** GET reads without mutation or request-body dependence;
  POST invokes/submits; PUT replaces; PATCH partially updates; DELETE removes.
  Add a verb only with a real operation contract, input encoding, authorization
  and tests. Sharing the path does not mean accepting every verb for every task.
  No filesystem deletion, job submission or other capability becomes remotely
  available merely because a method exists in HTTP.

## Design or review the change

1. **Record the mapping.** State adapter + supported method → header destination/
   version → typed variant → binding → task/handler and allowed effects. Keep
   transport DTOs/codecs at the boundary. Capability-owned enums stay internal;
   a heterogeneous host may use a finite application-owned sum, never a framework
   business mega-enum, `serde_json::Value` or `Any` fallback. Parsing is bounded,
   side-effect-free and selected by explicit route/version information.
2. **Guard entry independently of routing.** Authenticate at the trusted adapter
   boundary and authorize at the destination before task/client effects. Current
   HTTPS subjects come from a verified TLS leaf plus explicit identity mapping,
   never an HTTP/body identity claim. A trusted `dispatch_verified` caller is not
   itself proof of authentication; do not expose it to arbitrary unverified claims.
   Preserve target/incarnation, expiry and request/correlation semantics. Protected
   discovery/help also needs admission; route knowledge or a menu entry is not
   permission. Local trusted-owner selection is not production authentication.
3. **Assemble, do not locate services at runtime.** Bootstrap selects optional
   capability packages, passes their typed startup config and invokes public
   registrars. Registrars stage cohesive configured factory groups without resource
   construction; validate/freeze, then explicitly create and inject dependencies.
   Factories construct; providers own acquisition/reuse/leases; task bindings own
   registered routes. Factory-group registration and route registration are separate
   protocols, not a universal container or automatic route generator. Singleton
   creation and lazy transient acquisition have different timing. Use
   [provider wiring](../wire-forge-providers/SKILL.md) and
   [public capability delivery](../expose-forge-capability/SKILL.md) when relevant.
4. **Keep the adapter thin.** Tasks compute, prepare business payloads and orchestrate
   injected `IoClient` contracts; clients execute prepared requests plus boundary
   encoding/I/O. Pure tasks require no client. `Task` remains independent of HTTP,
   envelopes and `DescribeCommand`; command-facing descriptions are a separate
   contract. A new task needs explicit factory/route binding and codec/description
   updates where applicable—not another endpoint or controller with business logic.
5. **Preserve failure meaning.** Unknown routes, unsupported versions, denied
   admission and variant disagreement must reach no handler effects. A timeout
   after send can leave execution unknown; do not reset deadlines, silently retry,
   or equate request IDs with idempotency. Use [command tracing](../trace-forge-command/SKILL.md)
   for detailed hops, outcome semantics and actual effect boundaries.

## Counterexamples to catch

| Tempting change | Required correction |
| --- | --- |
| Add `/forge/tasks/render` and call `RenderTask` in its controller | Keep the gateway; assemble/register a typed destination and delegate through admission |
| Header says `jobs.submit`, body contains a status variant | Reject the mismatch; do not rewrite the header or run status instead |
| Trust `Forge-User: admin` or let payload override destination | Derive identity from the verified boundary; reject routing conflicts and independently authorize |
| Map GET, PUT and DELETE to the existing POST handler | Keep unsupported verbs rejected until their real semantics and allowed routes exist |
| Treat a factory lookup or example-only registrar as plug-and-play delivery | Export reusable capability integration; freeze before construction and prove normal consumer imports |

These are review scenarios, not assertions that jobs/render are currently exposed
by the remote gateway.

## Completion checklist

- [ ] One stable HTTP path; supported verb/route pairs are explicit, with no per-task bypass.
- [ ] Header/version/target checks and typed variant agreement precede handler effects,
      including direct in-process entry; no body-based routing or wire-type leakage.
- [ ] Verified identity and destination authorization are distinct; protected help
      does not bypass them, and unavailable remote discovery is not invented.
- [ ] Public registrar/factory/provider wiring is real, conflicts do not overwrite,
      and optional adapter dependencies do not contaminate core contracts.
- [ ] Relevant negative tests cover unsupported methods, bad/duplicate headers,
      unknown routes, mismatched variants, spoofed identity and zero handler calls;
      substitution preserves the contract rather than merely returning similar text.
- [ ] Report implemented versus proposed behavior and exact evidence. Docs-only
      changes need skill/link/scenario validation; do not claim unrun runtime tests.

Return **current slice; proposed mapping; assembly/ownership; admission and effect
boundaries; findings; checks and gaps**. Keep the answer scoped to the requested
change, not a plan to implement every future transport or HTTP method.
