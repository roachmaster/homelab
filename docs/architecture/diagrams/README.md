# Architecture diagrams

These diagrams show a **proposed reference application**, not verified deployed infrastructure.

| Diagram | Question | SVG | Editable source |
| --- | --- | --- | --- |
| C4 context | Who uses the assembled system? | [SVG](01-context.svg) | [DOT](01-context.dot) |
| C4 containers | Which application and data store run? | [SVG](02-containers.svg) | [DOT](02-containers.dot) |
| C4 components | What is inside the application host? | [SVG](03-components.svg) | [DOT](03-components.dot) |
| Package dependency DAG | What may import what at compile time? | [SVG](04-package-dependencies.svg) | [DOT](04-package-dependencies.dot) |
| Submission sequence | When is durable acceptance safe? | [SVG](05-submit-sequence.svg) | [DOT](05-submit-sequence.dot) |

C4 view boundaries and entity types are labeled directly. Teal highlights core policy/admission; amber marks composition/lifecycle; gray indicates external technology or systems. Dashed component arrows mean construction/lifecycle control; dashed context/container arrows mark an optional integration. Solid arrows mean the interaction/dependency named by the diagram, not a universal shared arrow meaning.

The crate DAG is not a C4 deployment view. Handler bindings and small driven adapters reside in `reference-app` boundary modules initially; they can be extracted when justified. The sequence depicts a successful new-key durable submission; rejection, replay, and failure semantics are described in the [blueprint](../forge-framework-blueprint.md).

Regenerate from the repository root with Graphviz installed:

```bash
for source in docs/architecture/diagrams/*.dot; do
  dot -Tsvg "$source" -o "${source%.dot}.svg"
done
```

Review the rendered images after changing source; successful rendering alone does not establish readable labels, correct boundaries, or sensible dependency direction. PNG previews can be rendered locally with `dot -Tpng` for visual QA; they need not be committed alongside the scalable SVGs.
