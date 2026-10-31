---
name: model-forge-module
description: Explain the evolving Forge composition DSL or model a capability/module, pure task, injected-client workflow or bootstrap/registrar assembly. Separate observed source patterns from proposed homelab semantics and syntax; this starter skill does not imply a DSL parser or runtime exists.
---

# Model a Forge module

**Provisional starter.** Read the [canonical DSL reference](../../../docs/architecture/framework-dsl.md) for shared vocabulary and status labels. Keep this repository-local skill with that reference; if it is missing, report the gap rather than inventing a DSL.

## Implemented task/client pattern

[Task](../../../src/rust/forge-task/src/lib.rs) has associated `Input`, `Output`, `Error` and `run(&self, input)`. [IoClient](../../../src/rust/forge-io-client/src/lib.rs) has `Request`, `Response`, `Error` and `execute(&self, request)`. These synchronous contracts exist; the full composition DSL remains proposed. The contract crates have no normal dependency on each other. A consumer capability imports both only when its task uses clients; a pure task requires no I/O contract or envelope.

For command-facing names/arguments/help, use the separate [`DescribeCommand`](../../../src/rust/forge-command/README.md) contract, not a `Task` supertrait. A task is one execution stereotype; non-Task handlers can reuse descriptions and pure tasks need not provide them. Declare each enum operation separately, with named semantic argument types and required/optional/default meaning. Treat these as authored metadata, not reflection over `Task::Input`; derive registration/help from the same descriptor and test explicit codecs for drift. CLI help-option definitions stay in presentation, not business inputs.

For bootstrap work, read the [registrar implementation/reference](../../../docs/architecture/bootstrap-registrars.md). Model application-selected packages → typed startup config → registrar-staged cohesive factory groups → validation/freeze → explicit client/provider/task construction. Concrete clients, their dependencies/config/factories and reusable registrar live in optional implementation crates, never mandatory core. Contract-only crates need no empty registrar. The sixth core crate now owns typed factory-group tuples and metadata in a consuming registry; conflicts return the original builder. The optional demo has real package registrar functions. This is not automatic graph execution or a universal container.

For native system clients, use [`forge-kernel::disk`](../../../src/rust/forge-kernel/README.md) as the implemented optional-module example. Related low-level clients may cohabit cohesive modules; do not move concrete clients into the six core contracts or call this hosted crate a bootable kernel. Tasks prepare requests; filesystem execution and client-local cwd remain at the disk boundary.

For hosted-kernel expansion, consult the [research plan](../../../docs/architecture/forge-kernel-plan.md): the narrow local node/status slice now exists in `forge-kernel::node`, assembled by `forge-local` and exposed through `forge-terminal`. Inspect these APIs rather than extending their status into production health. Jobs and remote access remain proposals. Keep application jobs distinct from OS processes and local contracts distinct from remote failure/identity semantics; do not turn this plan into permission to implement later phases.

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
