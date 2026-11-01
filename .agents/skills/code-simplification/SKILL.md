---
name: code-simplification
description: Simplify a requested code slice for readability and maintainability while preserving its observable behavior and public contracts. Invoke explicitly for a scoped clarity refactor, not feature work or blanket architectural removal.
---

# Simplify code without changing its contract

Optimize for comprehension, not line count. Inspect the requested code, callers,
public exports, relevant history and tests before deciding that an abstraction or
branch is redundant. Leave already-clear or out-of-scope code alone.

## Select a small improvement

Identify the responsibility and the behavior that must remain fixed: inputs/outputs,
error types and timing, side-effect order, ownership/lifetimes, resource cleanup,
concurrency and relevant performance. Establish suitable baseline checks first.
Follow neighboring conventions rather than imposing a new style.

Good candidates include clearer names, named intermediate values, guard clauses
that preserve evaluation order, removing demonstrably dead private code, or a small
helper for a real repeated policy. Similar syntax alone does not justify a shared
abstraction. Numeric size/nesting/duplication thresholds are inspection prompts,
not mandatory extraction, deletion or automation rules. Automation is useful only
when its transformation and output can be reviewed and tested.

State what becomes easier to understand and why behavior is preserved. If that
argument depends on an unverified assumption, investigate or propose the change
rather than applying it. Keep bug fixes/features distinct in the diff; separate
commits or PRs only when the task and publication authority support that workflow.

## Preserve meaningful boundaries

In Forge, a single implementation can still justify a factory, provider, registrar,
client port or typed wrapper: it may encode construction timing, lifetime, admission,
consumer importability or substitutability. Do not replace real public integration
with example-only wiring, construct resources during registration, bypass the shared
header/variant gate, collapse typed payloads into JSON/Any, or move concrete I/O into
core simply to shorten code. Keep Task orchestration separate from prepared-request
I/O. When a boundary is the subject of the change, consult the existing
[package-boundary](../design-package-boundaries/SKILL.md) guidance and actual exports;
use the [capability delivery](../expose-forge-capability/SKILL.md) acceptance checks
when public integration is involved. These links do not authorize a broader redesign.

Avoid treating language examples as mechanically equivalent rewrites. Replacing
an object result with a Map changes its representation and callers. Removing an
async wrapper may change synchronous throws into a different promise/error contract,
identity or timing. Check the language and call-site semantics rather than copying
an example. Likewise, early returns must preserve cleanup and evaluation effects.

## Change and verify incrementally

Make one coherent transformation, inspect the diff, then run the checks that cover
its behavior. Preserve existing expected behavior; do not weaken tests to make the
refactor pass. Update tests tied only to a changed internal arrangement when justified,
but retain assertions on public contracts and meaningful effects. Recheck performance
when the altered path is sensitive.

If verification fails or clarity worsens, correct or undo only your own scoped edit
without discarding concurrent/user work. Review the final whole slice for accidental
API, dependency, error or lifetime changes. It is acceptable to recommend no change.

Return **scope; simplifications and rationale; preserved contracts; checks/results;
remaining risks**. An unrun check is a gap, not a pass. This skill does not itself
authorize commits, pushes, installations or unrelated cleanup.

Adapted under MIT; [pinned source, reconstruction and changes](references/provenance.md).
