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
4. Mark resource authorization and the first possible business effects. A task prepares typed client requests; the injected client performs the I/O. A composition binding may call `Task::run` without making the task depend on the envelope. Dependencies are constructor-injected; fan-out belongs to the use case, not the adapter or bridge.
5. State the typed outcome and actual completion/durable-acceptance point. Retry, partial failure and cancellation do not imply rollback or exactly-once effects. Contrast the relevant failure with the happy path.

Return **status/evidence; input; hops and checks; possible effects; outcome; proposed or executed verification**. A short path plus one counterexample is enough.

For a client-using task, trace **admitted binding → `Task::run` → prepared typed request → injected `IoClient::execute` → typed response/error**. The client may encode that prepared request for its boundary, but must not assemble business content. Use fake clients to check the exact prepared request, preserved error causes, no send after a failed fetch in a fail-fast flow, and no hidden retry. A fake is not proof of real I/O conformance; a pure task has no client hop.

Keep startup separate from this trace: registrars stage configured factories during bootstrap; request headers do not choose providers or carry startup configuration. Trace the already-assembled task/client binding, not a service-locator lookup. A presentation menu entry is not authorization; an action invoking application work still uses the admitted typed bridge. The implemented local terminal obtains descriptors through `system.commands`; its selected header/codec builds a typed command and `Application::invoke` uses the same bridge as direct `dispatch`. There is no terminal-to-handler shortcut. Its local admission policy is not production authentication.

For `help COMMAND` or `COMMAND --help`, trace admitted `system.commands` discovery → selected descriptor → terminal rendering, with **no business task/client execution**. Startup factories and console I/O are separate effects. The separate command-description trait does not force Task inheritance or help fields in typed input enums. Validate named semantic argument types/presence against actual codecs; metadata is not automatic deserialization, authorization or a reflection guarantee.

For native disk requests, trace the optional consumer’s bridge check before `ChangeDirectory` changes client-local state. A failed cd preserves cwd; readonly/mode metadata is not effective authorization. Use the [disk contract](../../../src/rust/forge-kernel/README.md) for symlink/native-path semantics, and distinguish real temporary-filesystem tests from fake task-substitution tests.

For implemented [local jobs](../../../src/rust/forge-local/JOBS.md), trace the shared bridge into owner-checked submit/inspect/list/step/cancel. Submit accepts volatile work; only explicit step runs one quantum. Running cancel is Requested, not confirmed stopped. Retained same-owner intent replay returns the receipt; request IDs do not deduplicate. Trusted in-process owner selection is not authentication.

## Small example

`jobs.submit @ 1` with `JobsPayload::Status(id)` is a mismatch. Reject before either handler's effects; never repair the header or dispatch status by inspecting the variant. Check that neither capability storage nor its per-operation acquisition is invoked.

For a requested adapter substitution, compare equivalent typed input, verified identity policy, preserved deadline/correlation/idempotency and outcome guarantees. Verify only the chosen adapter's actual parsing/lifecycle/settlement behavior; no second transport or broker mechanism is required. No implementation or infrastructure execution is implied by tracing the design.

For HTTP entry architecture or a proposed method/destination, use
[Design Forge Front Door](../design-forge-front-door/SKILL.md). It owns the
single-gateway review workflow and current-versus-future method distinction;
this skill owns the detailed request/effect trace. Verify actual projection in the
[HTTP adapter contract](../../../src/rust/forge-remote-https/README.md#http-header-projection).
