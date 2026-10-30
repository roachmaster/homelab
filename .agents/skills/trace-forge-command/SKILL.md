---
name: trace-forge-command
description: Explain or review a Forge typed command through canonical header routing, shared admission, registered binding, handler and effects. Use for variant mismatches and adapter substitution, not dependency resolution or arbitrary message-bus design.
---

# Trace a Forge command

**Provisional starter.** Read the [canonical DSL reference](../../../docs/architecture/framework-dsl.md), which owns vocabulary/status labels. Keep this repository-local reference available; do not invent missing APIs.

## Workflow

1. Record input, header destination/version, payload enum/variant and verified context. For a real review, follow actual entry-point, mapping, registry and handler code; distinguish observed behavior from proposed semantics.
2. In-process callers construct the enum directly. A selected adapter uses a bounded side-effect-free codec chosen by the registered header/version mapping. Serialization stays outside; payload contents do not choose the route.
3. Trace **header → registered destination/version → authorization/deadline → variant check → binding → capability**. Direct typed callers must not bypass admission. Explain the application-owned sum if one registry combines capabilities; no JSON/`Any` escape.
4. Mark resource authorization and the first possible business effects. Dependencies are constructor-injected; fan-out belongs to the use case, not the adapter or bridge.
5. State the typed outcome and actual completion/durable-acceptance point. Retry, partial failure and cancellation do not imply rollback or exactly-once effects. Contrast the relevant failure with the happy path.

Return **status/evidence; input; hops and checks; possible effects; outcome; proposed or executed verification**. A short path plus one counterexample is enough.

## Small example

`jobs.submit @ 1` with `JobsPayload::Status(id)` is a mismatch. Reject before either handler's effects; never repair the header or dispatch status by inspecting the variant. Check that neither capability storage nor its per-operation acquisition is invoked.

For a requested adapter substitution, compare equivalent typed input, verified identity policy, preserved deadline/correlation/idempotency and outcome guarantees. Verify only the chosen adapter's actual parsing/lifecycle/settlement behavior; no second transport or broker mechanism is required. No implementation or infrastructure execution is implied by tracing the design.
