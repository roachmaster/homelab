---
name: model-forge-module
description: Explain the evolving Forge composition DSL or model a capability/module and its assembly. Separate observed source patterns from proposed homelab semantics and syntax; this starter skill does not imply a DSL parser or runtime exists.
---

# Model a Forge module

**Provisional starter.** Read the [canonical DSL reference](../../../docs/architecture/framework-dsl.md) for shared vocabulary and status labels. Keep this repository-local skill with that reference; if it is missing, report the gap rather than inventing a DSL.

## Workflow

1. Identify the requested explanation, feature or review slice and its success guarantee. Inspect actual APIs/manifests when supplied; do not describe proposed names as existing exports.
2. Name the capability, owned payload enum/result and consumer-owned ports. Map header destination/version → expected variant → binding → use case. For task/client modeling, keep pure computation free of I/O requirements. Tasks prepare business requests and orchestrate injected typed clients; clients execute prepared requests and own only boundary encoding/I/O, not business payload assembly.
3. Describe required dependencies, selected factory/provider, typed config, override points and lifecycle owner. Composition wires; it does not perform the business workflow.
4. Keep package imports, assembly/lifetime order and runtime request flow distinct. Use modules for small internals; neither a module nor a crate automatically means a service.
5. Preserve typed shared ingress and transport neutrality. One heterogeneous registry needs a finite application-owned payload sum and explicit result unification, not a framework-global enum or JSON fallback.
6. State the smallest proof and gaps: successful typed call, rejection before handler effects, assembly failure without partial publication and a relevant substitution. Label proposed checks versus executed evidence.

Deliver a short model: **status/evidence; capability; routes; required/provided dependencies; assembly order; package boundaries; checks/gaps**. For a simple explanation, one example is enough. Use [module authoring](../../../docs/architecture/module-authoring.md) only when more detail is needed.

## Small example

Adding `notifications.send` beside jobs means a notifications-owned enum and `Delivery` port, a registered header/variant binding, and explicit dependency wiring. If they share one ingress, the application may extend its local `AppPayload` sum. Do not change bridge business logic or add a transport just to demonstrate extension. Clarify whether success means delivered, attempted or durably queued when that changes the model.

Any DSL spelling remains illustrative until verified against implementation. This skill does not authorize code generation, source refactors, installation or publication beyond the current request.
