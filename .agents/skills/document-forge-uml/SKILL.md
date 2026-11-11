---
name: document-forge-uml
description: Create or review Forge UML-style architecture views using actual Rust contracts or explicit proposals. Use for class, sequence, state, package, component and deployment diagrams and their consistency with requirements.
---

# Document Forge with UML

Use the [analysis and design guide](../../../docs/architecture/analysis-and-design.md) for study provenance and a worked Forge lifecycle. Start with the question the reader needs answered and identify the source or requirement behind the important relationships.

- For a class view, use the actual type or trait where practical. Mark trait implementation, ownership, borrowing, use and construction distinctly. Add multiplicities where they clarify capacity or lifetime. Mark simplifications such as omitted generic parameters. A lease can restrict validity without owning the provider.
- For a sequence, follow one command through its admitted path. A CLI or HTTP adapter constructs the typed request. The shared bridge resolves the header, admits the request and checks its variant before invoking a binding. Show rejection before handler effects. Use [command tracing](../trace-forge-command/SKILL.md) for the exact order and [provider wiring](../wire-forge-providers/SKILL.md) for acquisition and release.
- For a state diagram, name the object and record event, guard, state update and effect. A request to cancel a running job does not establish a terminal result. Separate business state from delivery, caller or provider state.
- For a package diagram, use Cargo dependencies and identify optional or development edges where relevant. For a component view, show supplied and consumed contracts. For deployment, identify actual processes or hosts and their communication. Use the requested C4 level when the task asks for C4.

Follow the editable [diagram gallery](../../../docs/architecture/diagrams/mermaid/README.md) conventions. Label observed behavior, proposals and illustrative abstractions. Check the same operation names, authorization boundary, state owner and success/failure ordering across the chosen views. When publishing a rendered diagram, inspect its labels and edges using the repository's rendering workflow.
