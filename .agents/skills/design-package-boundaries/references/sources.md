# Sources and design rationale

This is a practical synthesis, not an official implementation guide or a reproduction of either source. It has three distinct inputs:

1. **User preference:** a generic, package-oriented approach, as hexagonal as practical, that does not put everything into one package. This sets the default architectural direction, not a fixed package count or requirement for separate services.
2. **Clean Code grounding:** cohesion, reasons to change, honest interfaces, external boundaries, construction versus use, tests, incremental refactoring, and restraint in abstractions.
3. **Hexagonal supplement and implementation synthesis:** ports/adapters, inward package dependencies, consumer-owned external contracts, concrete package graphs, transaction/lifecycle checks, and language mappings. These are not all claims made by *Clean Code*.

## Clean Code study grounding

Robert C. Martin and Object Mentor contributors, *Clean Code: A Handbook of Agile Software Craftsmanship*, first printing July 2008, copyright 2009, ISBN 978-0-13-235088-4. The companion notes are original first-pass study commentary, not a transcription or exhaustive substitute for the book.

Pinned companion collection: [study index](https://github.com/roachmaster/study-notes/tree/711a5638662a3db7cb00a47c1514b748265b20a8/studies/clean-code), [source provenance and page map](https://github.com/roachmaster/study-notes/blob/711a5638662a3db7cb00a47c1514b748265b20a8/studies/clean-code/source.json). The study repository is private; access requires permission. These links are traceability aids, not runtime dependencies of the skill.

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

The book's Java-era examples do not mandate Java exception policy, inheritance, AOP, a DI container, or matching syntax in Rust/C. The skill adapts their broader design concerns and retains the notes' warnings against automatic over-extraction.

## Hexagonal architecture supplement

Alistair Cockburn, [Hexagonal architecture](https://alistair.cockburn.us/hexagonal-architecture), original article dated 2005. Its application boundary allows meaningful behavior to be exercised without the UI or database. Ports represent purposeful conversations; adapters translate between those conversations and external technologies. Different adapters may serve a port. The hexagon does not prescribe six ports.

The capability-first package layout, split/keep criteria, DAG review, transaction checklist, migration procedure, and Rust/Java/C mappings here are **our practical design synthesis**. Cockburn's model does not prescribe this directory structure, package count, or deployment arrangement. Use the source to understand the boundary idea, not as proof that every project needs the worked example's topology.
