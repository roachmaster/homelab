# forge-bitwave-system — the header-and-command idea is here, but the platform is unfinished

## Decision

**Adapt the message/command/registry separation; do not adopt this workspace as the new framework base.** This repository contains a recognizable ancestor of the desired shared logical bridge: a message has headers and payload, its type selects a registered command, validation happens before execution, and a domain adapter creates a concrete task. It also shows exactly how generic infrastructure can become entangled with a specialized workload.

This is a read-only architectural review of the default `main` snapshot observed on 2026-10-30. The exact revision, private source links and remote evidence are retained separately. Public locators below identify inspected files without copying source or exposing private operational details. No compilation, test execution, generation macro, benchmark or repository mutation was performed.

## What is actually assembled

The workspace explicitly lists five members: `forge-template`, `bitwave-core`, `forge-proc`, `forge-traits` and `forge-file-util`. `bitwave-model` is also a local dependency; it should not be dismissed merely because it is absent from that explicit list. `bitwave-commands` exists as a small scaffold rather than an assembled command application. A large `history/` tree contains older bridge, memory, GPU and prototype systems. The current README is minimal, so source and manifests are more reliable than an inferred grand design. (B1)

The principal responsibilities are:

- **`forge-traits`:** message contract, lightweight validation, command trait, a reusable crate-registry macro and generated message builders.
- **`forge-template` / `forge-proc`:** template rendering and compile-time generation from schemas/YAML.
- **`forge-file-util`:** converts messages into model-file tasks and declares task action/registry seams.
- **`bitwave-model`:** a common view over supported model formats and tensor data.
- **`bitwave-core`:** bit-wave representation, encoding/mechanics, analytics and several command-line programs.

The generic-looking crates are not currently lightweight ports. `forge-traits` depends on the procedural-macro crate, which depends on the template engine and its serialization/template stack. The domain core directly depends on analytics, columnar-data, CLI, parallelism and model packages. Separate folders have not prevented dependency coupling. (B1, B6)

## Two different flows exist

### The reusable idea: message to registered command

The `Message` trait separates a name, header map and payload map. The command trait supplies a command name, schema and `run` operation. The gatekeeper reads `header.type`, looks up the registered command, obtains a schema from a supplied loader, validates the message and only then calls the command. This closely matches the intended logical sequence: **canonical envelope → shared bridge → registered dispatch → domain action**. (B2, B3)

The evidence does not establish a completed transport-independent public gateway. The gatekeeper is crate-private, uses a string/map-based envelope, and receives its schema separately even though the command already exposes a schema method. There is no demonstrated REST/Kafka normalization, authentication-context admission, deadline propagation or acknowledgment contract on this path.

### The actual workload: domain CLI to computational code

The inspected `ingest` executable parses its own CLI, resolves input and lookup-table resources, optionally creates a missing lookup table, then calls domain ingestion functions for single-file, per-token or parallel outputs. It does not enter the header-based command gateway. Likewise, the model-file adapter translates a `Message` into a `FileTask` carrying an export configuration; the inspected public `gguf_api` returns that task rather than proving a complete registered execution path. These are useful pieces, but the source does not support drawing one fully integrated application platform around them. (B4, B7)

This distinction matters for the new architecture: preserve the intended shared bridge, but validate it with two real ingress adapters and an executable domain path before calling it the common entry point.

## Important invariants and missing guarantees

**Some validation precedes execution, but it is explicitly provisional.** The schema validator checks required keys and a few primitive types. Unknown types are accepted, the message name is unused, and it is not a full JSON Schema implementation. That is adequate prototype behavior to study, not a safe universal validator. The new framework needs explicit canonical-header validation plus a registered domain payload decoder, with structured errors and schema/version compatibility rules. (B2)

**Registration is process-global and first-initialization wins.** The registry macro stores an immutable map in a `OnceCell`. Duplicate names overwrite prior entries; reading before initialization panics; subsequent initialization attempts do not rebuild the registry. This makes dependencies implicit and complicates isolated tests or multiple applications in one process. A consuming, application-owned builder with duplicate rejection—similar to the later HPVS pattern—is a stronger starting point. (B3)

**Handler lifetimes are implicit.** Registrations hold `Arc` trait objects with send/sync bounds, effectively singleton handler ownership. There is no general start/ready/drain/stop lifecycle, request scope or resource-pool contract in this registry. The framework should not equate “registered” with “initialized and healthy.”

**Generation depends on ambient repository state.** Procedural expansion reads `REPO_ROOT`, resolves schemas/templates and parses rendered text as Rust. A reusable package should have explicit, reproducible inputs rather than requiring a particular checkout environment. Optional code generation can be valuable, but a minimal message/handler contract should compile without the generator. (B6)

**Configuration is not yet robust application admission.** The file utility's configuration loader expects several environment variables and prints their values. Replace panic-on-missing configuration with typed startup errors and deliberate redaction policies. The technical point is portable behavior; no actual environment values are part of this public review. (B4)

**Template caching is global and unbounded.** It is keyed only by a hash of template text and rendering metadata, with no full-key collision check. Rendering also has custom missing-key, partial and whitespace policies. Keep this as optional developer tooling; do not smuggle global caching/template semantics into request dispatch. Any extraction needs explicit cache bounds, deterministic inputs and independent tests. (B5)

## Reuse disposition

| Candidate | Decision | New home |
|---|---|---|
| Header/payload separation | Adapt | Typed canonical envelope contract |
| Validation-before-command invocation | Reuse as an invariant | Shared logical bridge |
| Per-domain registration interface | Adapt | Explicit registrar contributing to an application-owned registry |
| Message-to-domain-task adapter | Adapt | Thin domain decoding boundary |
| Command schema metadata | Consolidate | One authoritative operation descriptor, not competing schema paths |
| Template rendering and builder generation | Optional/reference | Developer tooling or opt-in macro package |
| Global `OnceCell` registry and permissive schema stub | Replace | Tested startup builder and strict validation |
| Model formats, tensor adapters, wave mechanics and analytics | Exclude from generic core | Optional domain applications/adapters |
| Historical bridge/GPU/prototype trees | Historical reference only | Separate migration/design notes |

The proposed framework should distinguish **wire envelope**, **validated logical command**, and **typed domain request**. Transport adapters perform framing and transport-specific authentication/acknowledgment work; the bridge handles canonical policy and route resolution; the handler owns its domain decoding and orchestration. No transport should be a gateway into another transport, and no universal trait should expose GGUF paths or bit-wave representation.

A starter can provide an explicit composition root that registers two unrelated domains. Add REST and Kafka adapters to that same bridge, then prove that the operation identity, request/correlation context and error classification agree across transports. Keep broker commit/retry behavior at the Kafka adapter while making domain idempotency requirements explicit. These are proposed contracts, not implemented capabilities of this snapshot.

## Testing, CI and portability

Inspected message/registry tests cover basic lookup, successful validation, missing required data and unknown commands. The template tree contains tests for caching determinism, missing keys, partial overrides, rendering and whitespace policies. Domain code has its own test/benchmark material. **None of these tests were run during this review.** Small positive tests do not establish schema completeness, duplicate-registration behavior, concurrent application isolation or shutdown safety. (B8)

Remote metadata reports a completed **failed CI run at the reviewed revision**. Available job metadata had no retained step detail; the exact failure cause was not established. Independently, static workflow inspection reveals drift: the current CI workflow attempts a release build for `bitwave-bridge`, while that package is under `history/` rather than the inspected active dependency graph. The release workflow also expects that binary. These are observed configuration inconsistencies, not a claim that this was the historical failing step. Clippy was explicitly removed, and formatting is installed but not executed as a gate. (B9)

The full file tree and branch inventory were enumerated; only `main` was present. The review followed active framework-like packages and representative domain execution rather than exhaustively auditing archived implementations. No root project license or package license declaration was found. Extraction must not assume the historical models, third-party material and original code all have the same redistribution status.

## Evidence locators

| ID | Source path and lines | Supports |
|---|---|---|
| B1 | `Cargo.toml:1–13`; package manifests; `bitwave-commands/src/lib.rs:1–14` | Workspace, dependencies and scaffold |
| B2 | `forge-traits/src/msg/forge.rs:4–77`; `forge-traits/src/cmds/forge.rs:4–12` | Envelope, validator and command contract |
| B3 | `forge-traits/src/cmds/registry.rs:11–29`; `forge-traits/src/std/registry.rs:24–83` | Gatekeeper and global registry behavior |
| B4 | `forge-file-util/src/lib.rs:7–12`; `forge-file-util/src/gguf/adapter.rs:8–23`; `forge-file-util/src/config.rs:14–32` | Domain translation and startup configuration |
| B5 | `forge-template/src/engine.rs:16–95`; `forge-template/src/cache.rs:12–40` | Rendering and caching policy |
| B6 | `forge-proc/src/expand.rs:6–21`; `forge-proc/src/env.rs:4–8`; `forge-traits/Cargo.toml:6–12` | Ambient generation dependency |
| B7 | `bitwave-core/src/bin/ingest.rs:144–179` and subsequent mode branches; `bitwave-model/src/api.rs:43–58` | Direct domain flow and model port |
| B8 | `forge-traits/src/cmds/unit_tests/registry.rs:28–72`; `forge-traits/src/msg/unit_tests/msg.rs:46–103`; `forge-template/tests/` | Inspected test coverage |
| B9 | `.github/workflows/ci.yml:19–50`; `.github/workflows/release.yml:20–26`; authenticated run/check metadata | Workflow drift and failed run |
