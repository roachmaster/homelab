---
name: wrap-forge-process-exec
description: Route bounded subprocess execution in Forge crates through forge_work_worker::process_exec::SystemProcessExec (IoClient) and enforce the check_process_exec.py guard. Use when adding or refactoring subprocess calls in Rust Forge crates, choosing between bounded IoClient execution and streaming process_group(0) launchers, writing fake-IoClient unit tests, or updating check_process_exec.py.
---
# Wrap Forge Process Exec

Guide for routing bounded subprocess execution through `forge_core::io_client::IoClient` via `forge_work_worker::process_exec`, keeping subprocess callers unit-testable without spawning real commands, and preventing unapproved `std::process::Command::new` calls.

Start with [Wire Forge Providers](../wire-forge-providers/SKILL.md) and the assigned feature branch. Wrapping a command does not authorize executing it, starting models, accessing credentials, installing software or changing services.

## When to Use

- Adding any new bounded subprocess invocation in a Forge Rust crate
- Refactoring raw `std::process::Command::new` calls out of policy or adapter modules
- Injecting fake process executors (`FakeProcessExec` / `ScriptedExec`) into git runners, supervisor ports, observers, or AST parsers
- Reviewing or updating `src/rust/scripts/check_process_exec.py`

## Steps

### 1. Decide Bounded `IoClient` vs. Streaming Launcher

- **Use `forge_work_worker::process_exec` for simple finite trusted commands:** It takes `argv`, an explicit `EnvPolicy`, optional `stdin`, and integer `timeout_seconds`, and returns `ProcessOutput { exit_code, stdout, stderr }`. The current adapter kills/waits only its direct child; timeout is clamped to at least one second. It has no cwd/cancellation/output-byte-limit/process-group contract. Output is captured without a byte cap, and pipe-reader completion can outlive the direct child. Do not use it as a hard total-deadline or descendant-stop proof for untrusted candidate/model commands.
- **Preserve the existing stronger execution port:** Streaming, group cancellation/reaping, explicit cwd, bounded output or privilege dropping need a port that actually enforces them. Inspect the selected `RunnerPort`/`SupervisorPort` or publication `CommandExecutor` before substituting it. If a required guarantee is missing, report it and implement the selected typed port only within authorized scope; do not add a raw-spawn allowlist waiver or weaken supervision.

### 2. Construct Typed `ProcessRequest` Values

- Use `ProcessRequest::cleared(argv, timeout_seconds, retain, set)` whenever calling git or security-sensitive helpers so ambient environment variables are stripped (`env_clear()`) and only allowlisted keys (such as `PATH`, `LANG`, `LC_ALL`, `SYSTEMROOT`) plus explicit overrides (`GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_TERMINAL_PROMPT=0`) reach the child.
- Use `ProcessRequest::inherited(argv, timeout_seconds)` only when the child genuinely needs the host environment (such as `systemctl` supervision queries).
- Chain `.with_stdin(bytes)` when piping input to the child; `SystemProcessExec` writes stdin and drains stdout/stderr on dedicated threads. This does not establish bounded stream memory or group cleanup. Preserve caller cwd through a port that supports it, and derive timeouts from the remaining caller deadline without resetting budgets on retries.

### 3. Build Generic Adapter + Production Default

Make every consumer testable without spawning real binaries:

1. Write a generic adapter `pub struct FooAdapter<C> { client: C }` bounded on `C: IoClient<Request = ProcessRequest, Response = ProcessOutput, Error = WorkerError>`.
2. Keep the zero-config production type (for example `CommandGitRunner`, `SystemCommandPort`, `ProcessObserverExecute`, `PythonAstPort`) as a thin wrapper delegating to `FooAdapter::new(SystemProcessExec)`.
3. For bootstrap composition, stage `forge_work_worker::process_exec::register_process_exec()` (provides `forge.work_worker.process_exec.client`) before dependent groups that declare it in `requires`.

### 4. Test and Run Guards

- Write unit or `forge-scene` BDD tests over an in-memory `IoClient` double asserting exact `argv`, `EnvPolicy`, `timeout_seconds`, nonzero exit handling, and timeout error translation.
- Run the guards:

```bash
cargo test --manifest-path src/rust/Cargo.toml -p forge-work-worker -p forge-work-publication
python3 src/rust/scripts/check_process_exec.py
python3 src/rust/scripts/check_core_layers.py
```

## Gotchas

- Never index `&argv[0]` directly before validating; `SystemProcessExec` rejects empty `argv` or empty program strings with `WorkerError::Protocol` before spawn.
- Running `check_process_exec.py` rejects new raw `Command::new` sites outside its approved paths/counts. This validator exists as a manual command, not a currently wired CI gate; its pass is not a subprocess safety proof or permission to change the allowlist.
- When migrating an existing module off `std::process::Command`, remove any now-unused `std::io::Write` or `std::process::*` imports so `-D warnings` stays clean.

## Current References

Inspect [process_exec.rs](../../../src/rust/forge-work-worker/src/process_exec.rs), [injected process/Git ports](../../../src/rust/forge-work-worker/src/ports_runtime.rs), [publication CommandExecutor](../../../src/rust/forge-work-publication/src/publication_clients.rs), [group termination/reaping](../../../src/rust/forge-work-worker/src/process.rs), the [process-exec BDD binding](../../../src/rust/forge-scene/tests/bdd_process_exec.rs), and the [manual guard](../../../src/rust/scripts/check_process_exec.py). Run `make check-imports` and `make check-deps` for changed boundaries. Preserve nonzero-exit handling: ProcessOutput with a nonzero exit_code is not automatically an Err.
