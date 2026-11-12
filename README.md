# Homelab

A professional engineering portfolio: lessons from the Forge experiments, a growing composable Rust framework, and reusable design practices.

## Forge framework portfolio

- [Documentation index](docs/README.md): observed repository findings versus proposed architecture.
- [16 repository reviews](docs/reviews/forge/README.md): reusable ideas, risks, and boundaries.
- [Framework blueprint and assembly model](docs/architecture/forge-framework-blueprint.md).
- [C4 and supporting diagrams](docs/architecture/diagrams/README.md).
- [Forge IDE UI proposal](docs/architecture/proposals/forge-ide-ui.md) · [earlier PDF snapshot](docs/architecture/proposals/forge-ide-ui.pdf) · [six Mermaid/SVG views](docs/architecture/diagrams/mermaid/README.md#forge-ide-ui-proposal) — original proposal, now followed by the [generated inspection UI](src/rust/forge-ide-web/README.md).
- [Remote Forge IDE: implementation and original proposal](docs/architecture/proposals/remote-ide.md) · [visual PDF](docs/architecture/proposals/remote-ide-proposal.pdf) · [five Mermaid/SVG views](docs/architecture/diagrams/mermaid/README.md#remote-ide-proposal).
- [Detailed future jobs, access and remote interfaces](docs/architecture/foundation-interfaces.md) — proposed modules, typed contracts and ownership/state diagrams.
- [Implementation roadmap](docs/roadmap/forge-framework.md).
- [Hosted kernel research and phased plan](docs/architecture/forge-kernel-plan.md): implemented local node/status and bounded jobs, plus a narrow authenticated network proof; remote deployment remains future.

The [first Rust core](src/rust/README.md) now implements typed contracts, a synchronous shared bridge, factory/provider/lease primitives, independent I/O-client/task contracts, and atomic typed bootstrap registration with runnable optional-package consumers and focused tests. An optional [forge-kernel disk module](src/rust/forge-kernel/README.md) adds native filesystem inspection and client-local navigation without shell commands. It is a hosted system-client crate, not a bootable OS. The larger framework and durable reference application remain proposals; earlier repository reviews are not fresh build/test certifications.

## Forge GPU inference

The [SYCL llama.cpp adapter](src/rust/forge-llama/README.md) exposes authenticated loopback REST commands to start, switch, prompt and stop a local model on Forge GPU.

The [Mission Control runner](deploy/mission-control/agent-runner/README.md) records Chat requests and events in PostgreSQL and sends inference requests over the private Forge tunnel. See the [service inventory](docs/architecture/forge-ide-system-inventory.yaml) for the deployed endpoints.

## Forge IDE read-only workbench

[![Proposed read-only IDE workbench](docs/design/forge-ide-wireframes/forge-ide-workbench-desktop.svg)](docs/design/forge-ide-wireframes/README.md)

The [generated workbench](src/rust/forge-ide-web/WORKBENCH.md) opens a single allowed project automatically and provides project-wide file/symbol search, Explorer, Outline and a read-only source viewer. Opening a file reads bounded source through the admitted host and agent; its SHA-256 must match the bytes used for inspection. Selecting a symbol reveals its numbered source lines, including on phones. Problems, metadata and source-shape analysis remain supporting tools. The [original wireframes](docs/design/forge-ide-wireframes/README.md) above are design images, not screenshots. Editing, saving and Run/Debug remain future capabilities.

[Component, page and transition contracts](docs/design/forge-ide-components/README.md) describe the earlier nine-composition workbench. The current [generated IDE](src/rust/forge-ide-web/README.md) opens to Chat and uses local WASM ABI 4. Rebuild its assets as a bundle. The native gateway admits `ide.source` separately and validates inspected-content digests, so deploy the host and agent together. Native/WASM checks and real-browser desktop/narrow acceptance remain distinct gates.

## Forge IDE generated browser boundary

[![Generated UI build and runtime ownership](docs/architecture/diagrams/mermaid/images/m25-ui-implementation.svg)](src/rust/forge-ide-web/README.md)

The [earlier UI report snapshot](docs/architecture/proposals/forge-ide-ui.pdf) uses Mission Control as a guide, not a source of truth: Rust-owned state, thin worker/DOM adapters, and an explicitly secured host bridge to the existing agent. The [new implementation](src/rust/forge-ide-web/README.md) adds a generated inspection UI and loopback-bound host. Explicit HTTPS-origin configuration supports an access-controlled proxy with Secure cookies and optional automatic sessions using the proxy's existing sign-in; see [deployment and rollback](src/rust/forge-ide-web-host/deploy/README.md). It remains a single-operator read-only workbench. The [current UI skill](.agents/skills/design-forge-ui/SKILL.md) and [updated proposal](docs/architecture/proposals/forge-ide-ui.md) supersede its manual-asset authoring advice: application HTML/CSS/JS must be generated from declarations.

## Run the local Forge system

From the repository root with stable Rust, GNU Make and Python 3.9+. UI generation/CI also requires Node20+ and the `wasm32-unknown-unknown` Rust target:

```bash
cargo fetch --manifest-path src/rust/Cargo.toml --locked # once for dependencies
make                         # discover the available targets
make run ARGS='node.info'
make run ARGS='disk.list --help' # typed command arguments and defaults
make session                 # local stateful terminal
make run ARGS='jobs.submit --help' # bounded local counter
make run NO_DEFAULT_FEATURES=1 ARGS='node.status'
make test                    # workspace tests
make ci                      # native + actual WASM/generation gates
make ui-build                # generated assets; requires WASM target + Node
make ui ARGS='--help'         # configured loopback IDE web host
make ui-qa                   # build + sandboxed headless Chrome QA and reports
make asm                     # release assembly for the kernel library
```

The [root Makefile](Makefile) wraps Cargo in `src/rust/`; [target/feature selection,
assembly output and CI details](src/rust/README.md#root-makefile) include the direct
Cargo equivalents. The workflow performs CI checks only: no deployment or release
publication is configured.

[Headless IDE QA](src/rust/forge-ide-web/tests/browser/README.md) reuses the 28-group
real-browser workbench suite with stable scenario IDs, JSON/Markdown/JUnit results,
sanitized screenshots and independent cleanup reporting. Install its documented
browser prerequisites first; `make ui-qa` does not install or weaken browser security.
Native/unit checks and injected faults remain distinct from actual browser-to-agent
acceptance. Each run gets fresh ignored output rather than reusing stale evidence.

The terminal discovers its selected registered commands and sends typed requests through the shared bridge. A separate [command-description contract](src/rust/forge-command/README.md) supplies names, help, named argument types and required/optional/default declarations; `Task` remains an independent execution stereotype. `help COMMAND` or `COMMAND --help` describes an admitted catalog entry without running the business task/client. In a session, use `disk.pwd`, `disk.list`, `disk.cd PATH`, `disk.permissions PATH`, `node.info`, `node.status`, `help`, then `exit`. Directory changes persist only in that session, not the process or your parent shell. Paths in session input are literal remaining text—not shell quoting or expansion. [Usage, feature selection and exit codes](src/rust/forge-terminal/README.md) · [Application assembly](src/rust/forge-local/README.md).

**Implemented:** local node observations, command discovery, optional native disk navigation and [bounded local counter jobs](src/rust/forge-local/JOBS.md). In one session use `jobs.submit KEY TOTAL`, copy its ID, then `jobs.inspect ID`, `jobs.step ID`, `jobs.cancel ID` and `jobs.list`. Only explicit stepping runs work; receipts are volatile and local owner labels are not authentication. Node status reports client-instance uptime/responding, not machine health. **Later:** durable jobs, remote execution and Raspberry Pi deployment.

## First authenticated network boundary

The optional [remote node-info slice](docs/architecture/remote-node-info.md) now
sends typed `node.info` through one `/forge/v1/dispatch` HTTPS gateway, with routing
metadata in HTTP headers, mutual TLS and separate destination
operation authorization, then the existing shared bridge. The original example remains a **loopback proof**. The separately configured
agent below adds read-only IDE inspection; no remote jobs or mutations are exposed.

```bash
make remote   # ephemeral in-memory demo identities; one read, then exit
```

HTTP/TLS/JSON and the private runtime stay outside core contracts. The client port,
task and typed results remain replaceable; gRPC or Bitwave is not required.
[Public adapter API and limits](src/rust/forge-remote-https/README.md) ·
[Mermaid source](docs/architecture/diagrams/mermaid/m12-remote-node-info.mmd) ·
[SVG](docs/architecture/diagrams/mermaid/images/m12-remote-node-info.svg).

## Configured remote Forge IDE

[`forge-agent`](src/rust/forge-agent/README.md) now assembles node information and
read-only project inspection through the same mutual-TLS gateway. Select a named
remote node and an authorized project alias, never a caller-supplied remote path.
The task/report remain typed; YAML is the CLI output boundary.

```bash
make agent # help without credentials or network I/O
make agent ARGS='serve --config /path/to/server.json'
make agent ARGS='node.info --config /path/to/client.json --node lab'
make agent ARGS='inspect --config /path/to/client.json --node lab --project workspace'
```

![Implemented configured agent layout](docs/architecture/diagrams/mermaid/images/m18-agent-implementation.svg)

[Configuration, security boundaries and limits](src/rust/forge-agent/README.md) ·
[Original visual proposal PDF](docs/architecture/proposals/remote-ide-proposal.pdf) ·
[Milestone status](docs/architecture/proposals/remote-ide.md) ·
[Mermaid source](docs/architecture/diagrams/mermaid/m18-agent-implementation.mmd).
Two real processes are tested on localhost with ephemeral credentials. A real
cross-machine deployment and production credential lifecycle remain untested;
no host configuration or deployment is performed by this repository.

## Optional template rendering

[`forge-template`](src/rust/forge-template/README.md) is a schema-checked,
bounded Mustache-like renderer with method chains and 38 builtins, plus an opt-in
[single-shot template CLI](src/rust/forge-template/CLI.md). It is not a project
generator or a command added to the separate Forge terminal.
Default builtins are pure; custom methods are explicit trusted callbacks.
It accepts capability-local typed values, returns a complete string or typed error,
and exposes reusable `forge_template::assembly` config/factory/registrar APIs.
Only bootstrap/provider contracts are unconditional dependencies; the render algorithm
remains pure. Its example consumes those exports, then adds application-owned
task/description/bridge wiring without forcing I/O or transport:

```bash
make test PACKAGE=forge-template
make template ARGS='render --template "Hello, {{name.to_uppercase()}}!" --text name=Forge'
cargo run --manifest-path src/rust/Cargo.toml --offline --locked -p forge-template --example registered_render
```

### Template architecture and render flow

These Mermaid views cover the optional renderer library and its development
example. The separate opt-in CLI now consumes the same public assembly API; its
[execution and ownership flow](src/rust/forge-template/CLI.md#execution-and-ownership)
is documented alongside usage.

![Implemented optional template renderer architecture](docs/architecture/diagrams/mermaid/images/m09-template-architecture.svg)

- [Architecture SVG](docs/architecture/diagrams/mermaid/images/m09-template-architecture.svg) · [Mermaid source](docs/architecture/diagrams/mermaid/m09-template-architecture.mmd)
- [Method-enabled render sequence SVG](docs/architecture/diagrams/mermaid/images/m10-template-render.svg) · [Mermaid source](docs/architecture/diagrams/mermaid/m10-template-render.mmd)
- [Detailed captions](docs/architecture/diagrams/mermaid/README.md#implemented-template-renderer) distinguish library behavior, trusted callbacks and example-only composition.

## Forge IDE — project inspection

The first [Forge IDE capability](src/rust/forge-ide/README.md) emits a read-only
project tree and Rust AST-derived declaration hierarchy as YAML. Traits, impls,
methods, types, modules, spans and line counts are inspected with a real parser;
this is not type checking or a full expression AST.

```bash
cargo fetch --manifest-path src/rust/Cargo.toml --locked # first dependency setup
make ide ARGS='inspect src/rust/forge-task'
```

![Implemented Forge IDE inspection flow](docs/architecture/diagrams/mermaid/images/m11-ide-inspection.svg)

[Mermaid source](docs/architecture/diagrams/mermaid/m11-ide-inspection.mmd) ·
[Scope, output, limits and public integration](src/rust/forge-ide/README.md).

## Framework diagrams

**Implemented contracts**, shown as UML-like Mermaid (Rust type parameters and lifetimes are explained in the captions):

![Implemented Forge contracts and disk examples](docs/architecture/diagrams/mermaid/images/m01-contracts.svg)

- [Contracts: SVG](docs/architecture/diagrams/mermaid/images/m01-contracts.svg) · [Mermaid source](docs/architecture/diagrams/mermaid/m01-contracts.mmd)
- [Bootstrap sequence: SVG](docs/architecture/diagrams/mermaid/images/m02-bootstrap.svg) · [Mermaid source](docs/architecture/diagrams/mermaid/m02-bootstrap.mmd)
- [Typed disk request: SVG](docs/architecture/diagrams/mermaid/images/m03-disk-request.svg) · [Mermaid source](docs/architecture/diagrams/mermaid/m03-disk-request.mmd)
- [Local checkpoint and planned phases: SVG](docs/architecture/diagrams/mermaid/images/m04-kernel-phases.svg) · [Mermaid source](docs/architecture/diagrams/mermaid/m04-kernel-phases.mmd)

The **broader foundation** has a proposed module/interface set, with the narrow local counter lifecycle now implemented:

- [Module boundaries](docs/architecture/diagrams/mermaid/images/m05-foundation-modules.svg) · [Typed ports](docs/architecture/diagrams/mermaid/images/m06-foundation-interfaces.svg)
- [Job lifecycle](docs/architecture/diagrams/mermaid/images/m07-job-lifecycle.svg) · [Remote admission sequence](docs/architecture/diagrams/mermaid/images/m08-remote-admission.svg)
- [Design and stability decisions](docs/architecture/foundation-interfaces.md) · [Rust interface sketches](docs/architecture/foundation-contracts.md)

M07 describes the implemented local counter lifecycle. The generic module/interface model and remote/authentication flows remain design artifacts, not completed services.

[Diagram captions and reproducible local rendering](docs/architecture/diagrams/mermaid/README.md) distinguish implemented code from the [kernel plan](docs/architecture/forge-kernel-plan.md). No remote transport or bootable OS is implemented by these diagrams.

## Source sets

- [Source-set guide](src/README.md): boundaries, supporting files, and how to add a tool.
- [Rust](src/rust/README.md): capability crates and internal modules.
- [Python](src/python/README.md): purpose-oriented projects and import packages.
- [Bash](src/bash/README.md): executable tools and reusable shell libraries.

## Skills

Repository skills live separately from application code in `.agents/skills/`.


### Explicit workflow skills

These three adapted workflow skills are **explicit-invocation-only** and local to
this repository; they are not installed globally. Each includes its upstream MIT
license and pinned reconstruction provenance.

- [Diagnose Bugs](.agents/skills/diagnosing-bugs/SKILL.md): build a symptom-specific reproduction, test causes and verify a scoped correction. Invoke `$diagnosing-bugs`.
- [Verify Before Completion](.agents/skills/verification-before-completion/SKILL.md): match final claims to current evidence and disclose unrun checks. Invoke `$verification-before-completion`.
- [Simplify Code](.agents/skills/code-simplification/SKILL.md): improve clarity without changing behavior, public integration or typed boundaries. Invoke `$code-simplification`.

### Forge design and implementation skills

- [Design Package Boundaries](.agents/skills/design-package-boundaries/SKILL.md): capability-first packages and practical hexagonal boundaries, without unnecessary layers.
- [Model Forge Modules](.agents/skills/model-forge-module/SKILL.md): explain the composition DSL and model a capability/module assembly.
- [Design Forge IDE UX](.agents/skills/design-forge-ide-ux/SKILL.md): design and review a task-oriented IDE, recognizable Explorer and dominant selected work area, with truthful capabilities and progressively disclosed analysis.
- [Design Forge UI](.agents/skills/design-forge-ui/SKILL.md): generate HTML/CSS/JS from YAML/Rust component declarations; separate main-thread presentation from worker/WASM logic.
- [Test Forge UI](.agents/skills/test-forge-ui/SKILL.md): reuse the existing real-browser QA harness, inspect rendered and keyboard behavior, and distinguish real-agent evidence from controlled probes.
- [Design Forge Front Door](.agents/skills/design-forge-front-door/SKILL.md): preserve one HTTP gateway and header-selected destinations, with current versus future method semantics.
- [Trace Forge Commands](.agents/skills/trace-forge-command/SKILL.md): follow typed header-directed admission, handlers and effects.
- [Wire Forge Providers](.agents/skills/wire-forge-providers/SKILL.md): specify typed factories, acquisition scopes, leases and lifecycle wiring.
- [Expose Forge Capability](.agents/skills/expose-forge-capability/SKILL.md): deliver public config/factory/registrar exports with an independent consumer proof, not example-only integration.

The framework-specific skills share one [DSL semantic reference](docs/architecture/framework-dsl.md). Keep these repository-local skills with their linked documentation; the broader DSL is an evolving Rust-embedded composition model, not an implemented parser; the linked core documents its real public API subset.

The UI/UX skills also include focused workbench-craft and generated-motion
guidance adapted from VoltAgent's design reference index, design-taste,
Impeccable and Emil Kowalski, with optional already-configured Playwright MCP
exploration in the QA skill. [Pinned sources, changes and licenses](docs/design/skill-sources.md)
explain the adaptation to Forge's coding philosophy. No upstream suite, launcher,
hook, MCP server or dependency is installed by these repository-local skills.

Examples: `Use $model-forge-module to explain the DSL and model a notifications capability.`

`Use $design-forge-front-door to review a new destination or input adapter without adding per-task endpoints.`

`Use $trace-forge-command to trace this request and identify rejection before effects.`

`Use $wire-forge-providers to specify the provider scope and cleanup for this dependency.`

`Use $design-package-boundaries to review this feature and propose cohesive packages, dependency rules, and a safe migration plan.`
