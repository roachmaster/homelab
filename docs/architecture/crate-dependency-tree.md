# Rust crate dependency tree and consolidation analysis

**Status: working todo list for the crate consolidation and CI revision,
2026-11-22. Traced against main `c738faa`. Crate formalization complete
22/22 (section 7); the `forge-core` merge is done (section 8); the
`forge-llama`, demo-crate, `forge-kernel`, and CI-matrix decisions below
are still open.**

## Todo

- [x] Decide the `forge-core` merge (eight contract crates into one) and the name — done, section 8
- [ ] Decide `forge-llama`: remove from the workspace, wire into the host, or keep deliberately
- [ ] Decide the demo crates: keep in the workspace for every CI combination, or gate them to a single job / separate example workspace
- [ ] Decide `forge-kernel`: into `forge-core` as feature-gated modules, or separate
- [x] Decide the layering enforcement: a layering test, or keep the eight crate boundaries — layering test, `forge-core/tests/layering.rs`
- [ ] Rationalize the CI feature matrix (step two, after the layout decision) — drafted in docs/architecture/ci-pipeline.md: the imports check is built and wired, the branch/main split and the three-combination cut are specified, Jenkins/GitHub changes still to implement

Built to answer: which crates are cohesive enough to merge into a higher-order
crate, as the first step toward revising the CI portion of the Forge build.
Method: every `Cargo.toml` in `src/rust/` parsed (workspace members, path
dependencies, workspace-inherited dependencies, features); LOC counted over
`.rs` files under `src/`, `tests/`, `examples/`, `benches/`. No `cargo tree`
invocation was needed — the internal graph is fully determined by the
manifests, and every internal reference resolves to a workspace member.

## 1. The workspace in numbers

- **26 workspace members** (23 crates, 3 examples), **91,503 lines** of Rust.
- **111 internal dependency edges.** Maximum dependency depth: **6 layers**.
- **6 top crates** with no internal dependents: `forge-agent`,
  `forge-ide-web`, `forge-ide-web-host`, `forge-terminal`, `forge-llama`, and
  the `forge-bootstrap-demo` example.
- **7 foundation crates** with no internal dependencies: `forge-bootstrap`,
  `forge-command`, `forge-ingress-contract`, `forge-provider`,
  `forge-io-client`, `forge-ide-ui`, `forge-game-runtime`.
- The IDE family (`forge-ide-ui`, `forge-ide-web-host`, `forge-ide`,
  `forge-ide-web`) is **53,683 lines, 59% of the workspace**.

## 2. The dependency tree, by layer

Depth is the longest path from a foundation crate. An arrow `A -> B` means A
depends on B.

### Layer 0 — foundation, no internal dependencies

| Crate | LOC | Dependents | Role |
| --- | --- | --- | --- |
| `forge-bootstrap` | 690 | 15 | Typed, effect-free registration and freeze (DI registrars) |
| `forge-command` | 633 | 5 | Static, transport-neutral command descriptions |
| `forge-ingress-contract` | 261 | 13 | Typed ingress request/response values |
| `forge-provider` | 223 | 17 | Typed construction and synchronous acquisition |
| `forge-io-client` | 86 | 15 | `IoClient` typed I/O boundary |
| `forge-ide-ui` | 20,505 | 2 | WASM model, serde-only |
| `forge-game-runtime` | 3,300 | 1 | Game engine runtime |

### Layer 1

| Crate | LOC | Depends on | Dependents |
| --- | --- | --- | --- |
| `forge-task` | 304 | io-client, provider | 11 |
| `forge-access` | 147 | ingress-contract | 2 |
| `forge-jenkins` | 2,260 | bootstrap, io-client, provider | 2 |

### Layer 2

| Crate | LOC | Depends on | Dependents |
| --- | --- | --- | --- |
| `forge-bridge` | 809 | ingress-contract, io-client, provider, task | 9 |
| `forge-kernel` | 1,307 | bootstrap, command, io-client, provider, task | 4 |
| `forge-demo-messages` | 410 | bootstrap, io-client, provider, task | 2 |

### Layer 3

| Crate | LOC | Depends on | Dependents |
| --- | --- | --- | --- |
| `forge-remote` | 670 | bridge, ingress-contract, io-client, task | 4 |
| `forge-jobs` | 1,190 | access, bootstrap, bridge, ingress-contract, provider | 1 |
| `forge-llama` | 1,462 | bootstrap, bridge, ingress-contract, provider | **0** |
| `forge-template` | 6,972 | bootstrap, bridge, command, ingress-contract, io-client, provider, task | 1 |
| `forge-local` | 4,236 | bootstrap, bridge, command, ingress-contract, io-client, jenkins, kernel, provider, task | 1 |
| `forge-demo-memory-client` | 416 | bootstrap, demo-messages, io-client, provider | 1 |

### Layer 4

| Crate | LOC | Depends on | Dependents |
| --- | --- | --- | --- |
| `forge-ide` | 8,172 | bootstrap, bridge, command, ingress-contract, io-client, provider, remote, task | 3 |
| `forge-ide-web` | 7,224 | bootstrap, game-runtime, ide-ui, provider, template | 0 |
| `forge-terminal` | 3,282 | access, bootstrap, ingress-contract, io-client, jenkins, jobs, local, provider | 0 |
| `forge-bootstrap-demo` | 1,905 | bootstrap, bridge, demo-memory-client, demo-messages, ingress-contract, io-client, kernel, provider, task | 0 |

### Layer 5

| Crate | LOC | Depends on | Dependents |
| --- | --- | --- | --- |
| `forge-remote-https` | 4,747 | bootstrap, ide, ingress-contract, io-client, kernel, provider, remote, task | 2 |

### Layer 6 — application tops

| Crate | LOC | Depends on |
| --- | --- | --- |
| `forge-agent` | 2,510 | bootstrap, bridge, command, ide, ingress-contract, io-client, kernel, provider, remote, remote-https, task |
| `forge-ide-web-host` | 17,782 | bootstrap, bridge, ide, ide-ui, ingress-contract, io-client, provider, remote, remote-https, task |

### Features

Nine crates define features; the rest have none. The feature-bearing crates
are the CI multiplier (see section 5): `forge-ide` (cli, managed-writes,
remote, write-contract, yaml), `forge-terminal` (disk, durable-jobs, jenkins,
jobs), `forge-local` (disk, jenkins, jobs), `forge-remote-https` (ide,
managed-writes), `forge-kernel` (disk, node), `forge-template` (cli),
`forge-ide-web` (build-assets), `forge-jenkins` (http),
`forge-bootstrap-demo` (disk, messages).

## 3. Findings

### 3.1 The contract layer is eight crates that behave as one

`forge-provider`, `forge-io-client`, `forge-command`,
`forge-ingress-contract`, `forge-task`, `forge-bootstrap`, `forge-bridge`, and
`forge-access` share one character: transport-neutral, no business enums, no
async runtime, no global registry, **zero external dependencies, zero
features**, 3,153 lines across 19 files. Sixteen of the eighteen other crates
depend on at least one of them; `forge-agent` alone depends on seven of the
eight.

The fine granularity is the dependency-inversion mechanism — a consumer takes
only the contract it needs. But at this size the granularity buys almost
nothing in build time: the whole layer is 3% of the workspace, and a consumer
that takes one contract usually takes several.

**Recommendation: merge the eight into one crate, `forge-core`, with one
module per current crate** (`provider`, `io_client`, `command`, `ingress`,
`task`, `bootstrap`, `bridge`, `access`). Measured effect: crates 26 -> 19,
internal edges 111 -> 40. The current graph is acyclic, so the module
structure preserves the layering — but only by convention. The honest
trade-off: the crate boundary currently **enforces** the layering (for
example, `forge-command` documents that it has no task, I/O-client, or
ingress dependency, and the compiler proves it). Merging loses that
enforcement. Mitigation: keep the modules private behind re-exports, and add
a layering test in the style of the existing `check_dependencies.py` /
boundary-hash tests that fails if a lower module imports a higher one.

### 3.2 `forge-llama` has no internal consumer

1,462 lines, 9 external dependencies, **zero internal dependents**. It is
referenced in design documentation (`forge-ide-web-host/src/design/forge_providers.rs`)
and in the Wire Forge Providers skill as the model-runtime provider example,
but no Rust code in the workspace depends on it. The host relays chat to a
local llama.cpp model without it. Either it is dead weight that CI builds in
every combination for nothing, or its consumer is missing. This needs
Leonardo's call: remove it from the workspace, wire it into the host, or keep
it deliberately as a standalone provider.

### 3.3 The demo crates are a CI question, not a cohesion question

`forge-demo-messages` (410), `forge-demo-memory-client` (416), and
`forge-bootstrap-demo` (1,905) exist to demonstrate the DI wiring; they have
no production dependents. They are cohesive as a group and should stay
together. The question is whether they belong in the workspace for **every**
CI combination, or in a separate example workspace (or a single gated CI job)
so the main matrix does not pay for them ten times over.

### 3.4 `forge-kernel` is the judgment call

1,307 lines, features `disk` and `node`, depends on five of the contract
crates. It is "optional hosted, low-level system clients grouped by cohesive
modules" — disk and node observations, not contracts. It could join
`forge-core` as feature-gated modules, or stay separate as the system-client
crate. Either is defensible; the deciding factor is whether the system-client
role deserves its own boundary. Recommendation: keep it separate for now —
its features give it a different shape from the featureless contract layer,
and it is the natural home if more system clients arrive.

## 4. Considered and rejected

- **`forge-remote` + `forge-remote-https`.** They look like a pair, but the
  split is the point: `forge-remote` (670) is the transport-neutral node-info
  conversation with no network, serialization, or TLS; `forge-remote-https`
  (4,747) is the implementation. Merging mixes the contract with the
  transport. The only form worth considering is a feature-gated merge
  (`https` as an optional feature of one crate), which preserves the layering
  through features instead of crates — but it complicates the feature matrix
  CI is trying to shrink.
- **`forge-jobs`, `forge-local`, `forge-template` into their single
  dependents.** Each has exactly one internal dependent, which looks like
  fragmentation, but each is a coherent capability: durable jobs, local
  application composition, the template engine. A single dependent means a
  leaf capability, not a fragment. Keep.
- **The IDE family.** `forge-ide-ui` is the WASM model and must stay separate
  (WASM target, serde-only). `forge-ide-web` generates assets, `forge-ide`
  is the core library, `forge-ide-web-host` is the server. These splits are
  target and architecture boundaries, not accidents. Keep all four.
- **`forge-agent`, `forge-jenkins`, `forge-terminal`.** Application tops and a
  coherent Jenkins client. Keep.

## 5. What this means for the CI revision

Consolidation is step one of the CI revision, not the whole of it. Merging
the contract layer takes the workspace from 26 crates to 19 and the edge
count from 111 to 40, which removes seven compile units from **every**
combination CI runs. But the bigger multiplier is the feature matrix: `make
ci` runs on the order of ten feature combinations (default, all-features,
no-default-features, the six minimal no-default profiles in
`scripts/dead_code.py`, wasm, release), and every combination rebuilds every
crate it touches. Consolidation shrinks the per-combination cost; it does not
shrink the number of combinations.

The CI revision itself — step two, after the layout decision — needs to
rationalize the matrix: which crates genuinely need all-features, which need
the minimal profiles, whether the demo crates build in every combination, and
whether `forge-llama` builds at all. Those decisions belong with the CI
design, not this analysis.

## 6. Open questions

- Confirm the `forge-core` merge, and the name (`forge-core`,
  `forge-contracts`, `forge-foundation`)?
- `forge-llama`: remove from the workspace, wire into the host, or keep
  deliberately?
- Demo crates: keep in the workspace for every CI combination, or gate them
  to a single job / separate example workspace?
- `forge-kernel`: into `forge-core` as feature-gated modules, or separate?
- Is the layering test (section 3.1) the right mitigation for losing the
  compiler-enforced crate boundaries, or is that enforcement worth keeping
  the eight crates separate?

## 7. Formalization patterns (2026-11-22)

Before consolidation, every crate was formalized to the framework shape:
`lib.rs` holds only `mod` declarations and `pub use` re-exports (plus the
crate doc header and crate attributes) — no traits, functions, structs, or
other item definitions. 22 of 26 crates needed it; `forge-ide-web`,
`forge-ide`, `forge-kernel`, and `forge-remote-https` were already clean.
All 22 are done on this branch, one crate per commit, public API
byte-identical everywhere, no behavior changes, every crate's tests passing
unchanged. The sweep found coupling, not bugs. The patterns below are the
record of what it found, and they matter for the consolidation step because
the same visibility questions recur when crates become modules.

### 7.1 Root-private items were hidden crate-wide coupling

Anything defined in `lib.rs` is visible to every submodule, so crates had
grown dependencies on private root items that nothing declared. Moving the
definitions into a module exposed each one, and the compiler found them all.
The fix everywhere was `pub(crate)` widening — the coupling was real, just
undeclared:

- `forge-game-runtime`: room fields, the `entity()` method, constants, and
  `RoomCollision` widened for `level.rs`, `queries.rs`, and the component
  tests.
- `forge-jenkins`: `job_path` and `Limits::validate` widened for
  `observation.rs` and `http.rs`.
- `forge-jobs`: `check_token`, `Work::validate`/`execute`, and `Owner`'s
  field widened for `store.rs` and `worker.rs`.
- `forge-template`: `Limits::validate` widened for `assembly.rs`.
- `forge-local`: `Bindings`, `Context`, `descriptor_for`, and
  `Bindings::register` widened for `disk.rs` and `jenkins.rs`.
- `forge-bootstrap-demo`: `stage_pure`, `bind_sum`, `publish`, the `Routes`
  alias, and `DemoAdmission` widened for `messages.rs` and `disk.rs`.

### 7.2 `pub(super)` only works while `lib.rs` is the parent

`forge-jenkins`' observation parsers were `pub(super)` — visible to `lib.rs`
and nothing else. Once the client code moved to `client.rs`, a sibling, the
parsers needed `pub(crate)`. Same shape as 7.1: visibility tied to module
position, not intent.

### 7.3 Sibling modules imported private root items through `super::`

`forge-local`'s `disk.rs` and `jenkins.rs` imported `descriptor_for` and
`Bindings` via `super::`; `forge-bootstrap-demo`'s `messages.rs` did the
same for the private staging helpers. After the move they import from the
new module (`crate::application`, `crate::demo`). Same hidden dependency as
7.1, different spelling — and it is invisible to a `crate::` grep, so the
check has to cover `super::` too.

### 7.4 Bare module paths need `crate::` prefixes after a move

Moved code referencing sibling modules (`jobs::`, `jenkins::`, `disk::`,
`messages::`, `parser::`, `schema::`, `render::`) had to become `crate::`
paths. Mechanical, but the replacement must be path-aware: a naive string
replace also rewrites `forge_jenkins::` and `forge_jobs::` (it did, once —
`forge_crate::jenkins::`). Match on a word boundary with no preceding colon.

### 7.5 Inline test modules travel with their definitions

`forge-game-runtime`, `forge-local`, and `forge-terminal` had inline
`#[cfg(test)] mod tests` using `use super::*;`, which pulls in the parent's
private imports as well as its items. Moving the tests with the definitions
kept them working (`game-runtime`'s went to a sibling file via `#[path]`).
One catch: imports used only by the tests (`forge-local`'s
`ArgumentDescriptor`/`DataType`/`Presence`) belong in the tests module, not
the moved module, or non-test builds fail on unused imports under
`-D warnings`.

### 7.6 The house pattern, now universal

`forge-remote-https` was already the exemplar: private concept-named modules
plus `pub use` re-exports at the root. All 26 crates now match it. Small
crates collapsed to exactly one concept-named module each (`task.rs`,
`client.rs`, `provider.rs`, `access.rs`, `bridge.rs`, `remote.rs`,
`messages.rs`, `memory.rs`, and so on); larger crates gathered their root
definitions into one new module beside the ones they already had.

### 7.7 What this means for the consolidation

The `pub(crate)` widenings in 7.1 are a map of the real intra-crate
coupling — the places where a module boundary is crossed in practice. When
the eight contract crates merge into `forge-core` (section 3.1), the same
questions recur at module level: which items the merged modules share, and
whether that sharing is declared or accidental. That is the concrete reason
the layering test matters: without it, a merge re-creates the root-private
pattern at a larger scale, with eight modules' privates silently visible to
each other through the crate root.

## 8. The `forge-core` merge (2026-11-22)

Done on this branch, in one commit. The eight contract crates are gone;
`src/rust/forge-core/` holds one public module per contract (`provider`,
`io_client`, `command`, `ingress`, `task`, `bootstrap`, `bridge`,
`access`), each moved from its crate with its crate doc as the module doc.
All 16 consumer crates now depend on `forge-core` alone; the old crates'
tests (82 tests + 6 doctests) and the two `forge-bridge` examples moved
with them, imports rewritten mechanically (`forge_task::` ->
`forge_core::task::`, and so on). No behavior changed, no public API
changed beyond the crate path.

Measured after the merge (same analyzer as section 1): **19 crates, 40
internal edges, max depth 4** (was 26 / 111 / 6) — exactly the section 3.1
prediction. `forge-core` is 4,135 lines over 22 files, 16 dependents, zero
external dependencies, zero features. Total workspace LOC is 96,213, up
from 91,503 — the growth is the test revamp and formalization work since
`c738faa`, not the merge.

Three things the merge turned up that section 3.1 did not foresee:

### 8.1 The modules are public, not private behind re-exports

Section 3.1 recommended private modules with re-exports. That does not
work: `forge-bootstrap` and `forge-bridge` both export a public `Registry`
(the factory-group registry and the frozen route registry), so a flat
re-export at the `forge-core` root would collide. The modules are public
and consumers qualify (`forge_core::bootstrap::Registry` vs
`forge_core::bridge::Registry`). The layering test is correspondingly more
important: it is the only thing stopping a module from reaching into a
higher one.

### 8.2 Per-crate optional dependencies are gone

Four consumers used the old crates as optional dependencies under
features: `forge-ide` (bridge/command/ingress under `cli`/`remote`),
`forge-template` (task/command/bridge/ingress/io-client under `cli`),
`forge-bootstrap-demo` (provider/io-client under `disk`/`messages`), and
`forge-terminal` (access/ingress under `durable-jobs`). One crate cannot
be both a required dependency (for the modules a consumer always needs)
and an optional one (for the modules it needs only under a feature), so
those consumers now depend on `forge-core` unconditionally and the
features gate only the *use* of the modules in code (`cfg(feature = ...)`),
not their compilation. The cost is compiling ~3,000 lines of zero-dependency
code in builds that previously excluded some of it — negligible. The
`check_dependencies.py` optionality assertions for those four consumers
were rewritten to match (the assertions on heavier optional dependencies —
serde, serde_yaml_ng, nix, forge-remote, forge-template — are unchanged).
`forge-ide-web` is the exception: its only core dependencies were
bootstrap/provider under `build-assets`, so its `forge-core` dependency
stays optional. `forge-template`'s `cli` feature is now an empty marker
feature, and the guard asserts it stays that way.

### 8.3 The architecture guard was rewritten for the new shape

`scripts/check_dependencies.py` hardcodes the workspace: its `ALLOWED`
map, the pure/messages/disk consumer profiles, the negative fixtures, and
the `cargo tree` assertions all named the eight crates. All of it now
names `forge-core` (fixtures: a `forge-core` -> `forge-kernel` edge, a
parser in `forge-core`, a `forge-core` -> `rustls` leak, a non-empty
template `cli` feature). The guard passes: 22 negative fixtures rejected,
all profile checks green. The `forge-core` layering test
(`tests/layering.rs`) is the module-level counterpart: it scans the eight
module sources for `crate::<module>` references and fails on any edge
outside the allowed set (task -> io_client/provider, bridge -> ingress /
io_client / provider / task, access -> ingress), and it fails if a module
is added without a layering rule. Both guards are part of the merge
commit, not follow-ups.
