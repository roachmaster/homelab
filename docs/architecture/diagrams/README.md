# Architecture diagrams

These diagrams show a **proposed reference application**, not verified deployed infrastructure. No input transport is selected or required; the first ingress proof uses an in-process driver. The component view shows the extension point for an input adapter only if a real consumer needs one.

| Diagram | Question | SVG | Editable source |
| --- | --- | --- | --- |
| C4 context | Who uses the assembled system? | [SVG](01-context.svg) | [DOT](01-context.dot) |
| C4 containers | Which application and data store run? | [SVG](02-containers.svg) | [DOT](02-containers.dot) |
| C4 components | What is inside the application host? | [SVG](03-components.svg) | [DOT](03-components.dot) |
| Package dependency DAG | What may import what at compile time? | [SVG](04-package-dependencies.svg) | [DOT](04-package-dependencies.dot) |
| Submission sequence | When is durable acceptance safe? | [SVG](05-submit-sequence.svg) | [DOT](05-submit-sequence.dot) |
| Provider/factory assembly | Who constructs, provides, injects and closes dependencies? | [SVG](06-provider-assembly.svg) | [DOT](06-provider-assembly.dot) |

C4 view boundaries and entity types are labeled directly. Teal highlights core policy/admission; amber marks composition/lifecycle; gray indicates external technology or systems. Dashed component arrows mean construction/lifecycle control; dashed context/container and package arrows mark optional integrations/dependencies. Solid arrows mean the interaction/dependency named by the diagram, not a universal shared arrow meaning.

The provider assembly view is a startup/ownership model, not another C4 container or runtime request router. It now shows bootstrap staging cohesive groups through registrars before construction; the host-owned singleton path is illustrated. Current transient acquisition is an alternative, not eager creation plus a second instance; pools and managed async lifecycle remain later. Dashed arrows in that view denote composition/lifecycle control. See [bootstrap registrars](../bootstrap-registrars.md) and [provider/factory DI](../dependency-injection.md).

The crate DAG is not a C4 deployment view. It distinguishes the five implemented contracts from the proposed bootstrap package and optional implementation crates. The latter can own clients, typed config, factory groups and registrars; the application selects them. Small non-reusable bindings/adapters can remain application modules. The diagram omits future runtime/lifecycle packages rather than making them prerequisites for the next slice. The internal envelope/bridge is generic over a capability-owned payload enum. Direct `Bridge<CapabilityPayload>` use requires that enum's owning crate to implement `Payload` and import the narrow ingress contract (the conditional edge shown). Alternatively the application owns a wrapper/sum implementing `Payload`; it cannot implement a foreign trait for another crate's enum. The header selects the registered route; variant mismatch is rejected before effects. Optional wire codecs live outside this typed path. The sequence depicts a successful new-key durable submission; rejection, replay, and failure semantics are described in the [blueprint](../forge-framework-blueprint.md).

Regenerate from the repository root with Graphviz installed:

```bash
for source in docs/architecture/diagrams/*.dot; do
  dot -Tsvg "$source" -o "${source%.dot}.svg"
done
```

Review the rendered images after changing source; successful rendering alone does not establish readable labels, correct boundaries, or sensible dependency direction. PNG previews can be rendered locally with `dot -Tpng` for visual QA; they need not be committed alongside the scalable SVGs.
