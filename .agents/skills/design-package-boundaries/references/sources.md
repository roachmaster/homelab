# Sources and design rationale

This is a practical synthesis, not an official implementation guide or a reproduction of either source. It has three distinct inputs:

1. **Skill convention:** a generic, package-oriented approach, as hexagonal as practical, that does not put everything into one package. Thin ingress adapters funnel through the same logical bridge path, with destination determined by header metadata and fan-out after admission. This sets the architectural direction, not a fixed package count, prescribed input transport, separate-service requirement, or vendor platform. Transport examples do not require adding those dependencies. Capability-owned internal payload enums and a generic typed envelope/bridge are also part of this convention; JSON is optional edge serialization, not the internal request representation. Header-selected routing validates enum-variant agreement before effects. Typed provider/factory dependency injection is a further design convention: separate construction, acquisition/lifetime and bootstrap wiring; inject narrow ports rather than a universal service locator. It does not mandate every scope or a container framework. The canonical envelope, safe registry and explicit admission contract operationalize this convention; the single bridge is not a universal requirement of hexagonal architecture or a claim from Clean Code.
2. **Clean Code grounding:** cohesion, reasons to change, honest interfaces, external boundaries, construction versus use, tests, incremental refactoring, and restraint in abstractions.
3. **Hexagonal supplement and implementation synthesis:** ports/adapters, inward package dependencies, consumer-owned external contracts, concrete package graphs, transaction/lifecycle checks, and language mappings. These are not all claims made by *Clean Code*.

## Clean Code study grounding

Robert C. Martin and Object Mentor contributors, *Clean Code: A Handbook of Agile Software Craftsmanship*, first printing July 2008, copyright 2009, ISBN 978-0-13-235088-4. The principles below are a practical synthesis, not a transcription or exhaustive substitute for the book.

| Study chapter | Printed pages | Principle carried into this skill |
| --- | --- | --- |
| 3 — Functions | 31–52 | Coherent abstraction levels, explicit effects, meaningful arguments; not universal line/argument ceilings |
| 6 — Objects and Data Structures | 93–101 | Representation hiding versus deliberate data exposure; separate transfer concerns when meaningful |
| 7 — Error Handling | 103–112 | Caller-actionable failure boundaries, context, cleanup, and normal absence versus failure |
| 8 — Boundaries | 113–120 | Consumer-specific external interfaces and tests against the real dependency; not wrappers around everything |
| 9 — Unit Tests | 121–133 | Readable, independent behavioral tests; unit doubles do not replace integration evidence |
| 10 — Classes | 135–151 | Cohesion, reasons to change, dependency inversion; extraction justified by change pressure |
| 11 — Systems | 153–170 | Construction separate from use, explicit dependencies, testable policy, incremental architecture |
| 12 — Emergence | 171–176 | Tested behavior, shared knowledge, intent, and avoiding needless classes/interfaces |
| 13 — Concurrency | 177–191 | Ownership, compound-operation invariants, lifecycle, and shutdown as explicit responsibilities |
| 17 — Smells and Heuristics | 285–315 | Evidence-led review, truthful interfaces, explicit dependencies, and judgment rather than mechanical rules |

The book's Java-era examples do not mandate Java exception policy, inheritance, AOP, a DI container, or matching syntax in Rust/C. The skill adapts their broader design concerns and avoids automatic over-extraction.

## Hexagonal architecture supplement

Alistair Cockburn, [Hexagonal architecture](https://alistair.cockburn.us/hexagonal-architecture), original article dated 2005. Its application boundary allows meaningful behavior to be exercised without the UI or database. Ports represent purposeful conversations; adapters translate between those conversations and external technologies. Different adapters may serve a port. The hexagon does not prescribe six ports.

The capability-first package layout, split/keep criteria, DAG review, transaction checklist, migration procedure, and Rust/Java/C mappings here are **our practical design synthesis**. Cockburn's model does not prescribe this directory structure, package count, or deployment arrangement. Use the source to understand the boundary idea, not as proof that every project needs the worked example's topology.
