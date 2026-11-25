# Source content and verification identity

Forge separates the work being requested, the source bytes being reviewed, and
the rules used to verify them. A task keeps its UUID when an agent resumes. The
source artifact has a canonical SHA-256 identity. A verification generation
binds that artifact to an immutable policy snapshot. Git commits, branches,
checkout locations and formatted patches remain useful provenance and delivery
coordinates.

**Source implementation, selected explicitly; live activation is separate.**
The new `content-v2` protocol is opt-in. Existing work keeps the
`legacy-v1` protocol and its original receipts. Native publication selects
`native_identity_protocol=content-v2` with an explicit UTC
`native_identity_after` admission cutoff. External workflow creation selects
`WorkConfig.identity_protocol=content-v2` and freezes it in workflow state.
This does not turn Jenkins,
GitHub pull requests or merging into a source-control-independent pipeline.
Those adapters still prove the exact Git candidate they build or publish.

## Three different identities

| Identity | Meaning | What must not change it |
| --- | --- | --- |
| Task/workflow UUID | The requested work and its authorization/lifecycle | Agent restart, checkout directory or retry |
| Artifact digest | Versioned canonical list of source paths, file modes, sizes and byte digests | Git commit metadata, branch name, repository object cache or diff formatting |
| Verification digest | Artifact identity plus frozen policy digest and explicit generation UUID | Current runtime installation or later configuration changes |

Changing any included path, executable mode or byte changes the artifact ID.
The same files acquired through memory, a directory or Git produce the same
artifact ID. Matching content does not grant another task's approval: workflow,
review, source-attempt, lease and context bindings still apply.

`ArtifactEntry(path, mode, size, sha256)` describes one regular file.
`ArtifactManifest.artifact_id` and `VerificationIdentity.verification_id` expose
the respective SHA-256 digests.
`ArtifactManifest(entries)` sorts its entries and exposes `canonical_bytes()`,
`to_dict()`, `from_dict()`, `from_bytes()` and `artifact_id`. The canonical
envelope uses the domain `forge.source-artifact`, version `1`, and a `files`
array. Paths are exact UTF-8 relative paths; modes are `100644` or `100755`.
The manifest contains file-byte digests rather than Git blob IDs. No absolute
checkout root, branch, parent commit or formatted patch is included.

`VerificationPolicySnapshot` retains the selected versioned policy bytes and
their `policy_sha256`. `VerificationIdentity(artifact_id, policy_sha256,
generation)` derives the verification ID. Generation is an explicit UUID;
creating a new verification context never mutates an old indexed receipt.

Native generations use the review UUID. External generations use the admitted
candidate-preparation action UUID. These are explicit scopes for verification,
not new task identifiers.

The [identity data view](https://portal.thedevex.com/design/content-identity/data.svg)
shows the concrete immutable values. Its editable Mermaid source and SVG are in
the repository diagram gallery.

## Source acquisition and ownership

Source adapters implement one bounded acquisition boundary. Bootstrap selects a
factory and injects its ready provider into the worker. Selection and config
validation precede I/O; acquisition performs the reads. The pure manifest and
policy values need neither a registry nor an empty factory.

Normal Git consumers receive
`GitArtifactProviderFactory(git, limits).create()` and call its ready
`GitArtifactProvider.acquire(repository, revision)`. The alternate
`ArtifactSourceFactory(config, git=ready_git).create().acquire()` accepts
`GitSourceConfig`, `DirectorySourceConfig` or `MemorySourceConfig`. These are
two acquisition shapes over the same pure artifact contract, not registries
consulted by business logic.

Memory, directory and Git adapters normalize to the same manifest contract.
Only regular files are admitted; links, submodules, duplicate paths, unsafe
relative paths and unsupported modes fail closed. Acquisition has entry, path,
file-size and total-byte bounds. Directory acquisition must reject a file that
changes during its guarded read. Git acquisition reads the selected revision's
blobs rather than a working directory's uncommitted files.

The directory adapter reads every regular file under its explicitly selected
plain/export root. It does not silently exclude `.git`, caches or untracked
files. Cross-source equality therefore compares the same selected file set;
pointing it at a full checkout with extra metadata intentionally changes the
artifact. File bytes, including newline conventions, are preserved exactly.

The worker owns its injected providers. No global registry, model process or
browser decides which source implementation or policy is authoritative. The
selected Git adapter may retain commit/tree/branch details beside the manifest;
those are provenance and continue to fence checkout and publication.

The [component and ownership view](https://portal.thedevex.com/design/content-identity/components.svg)
separates pure contracts, source adapters, durable snapshots and effectful
review/CI/publication ports.

## Freeze once, then execute

A content-v2 review obtains its selected artifact and policy snapshot before
the first check or branch/CI submission. The private durable envelope records
the selected protocol, artifact, policy and verification identity with its
review generation. Writing it successfully is a precondition for effectful
execution. A lease or admission failure rejects the operation before those
effects.

On resume, the worker loads and validates that envelope. It does not silently
enumerate the currently installed runtime, recompute a new verification ID and
claim it belongs to an old receipt. It still checks that the resumed candidate
contains the frozen source artifact. A missing, malformed or conflicting v2
snapshot stops the run; it does not fall back to v1.

Native composition constructs
`NativeVerificationIdentityFactory(NativeIdentityConfig(...), ready_artifacts)`.
The resulting `NativeVerificationIdentityProvider` owns `selected`, `freeze`,
`read`, `acquire`, `reference` and `plan`. `ReviewWorker` calls
`native.prepare_identity` before checks; native CI revalidates before a branch
push. The saved `forge-native-content-v2` envelope at
`<review UUID>/verification-identity.json` contains `scope`, `artifact`, `policy`,
`identity` and separate `provenance`. Its reference travels with the existing
native binding; hosted continuation keeps that reference. Saved state is loaded
before selecting current protected paths. An already recorded/indexed generation
or a queued review admitted before the configured cutoff remains v1.

External composition injects `ExternalCandidateIdentityProvider` with the same
ready artifact provider. Its `prepare` produces full candidate/base/protected
manifests, a versioned policy and verification identity. The private
`identity-manifest.json` is durable before the candidate bundle/import.
The candidate's `content_identity` reference carries `artifact_id`,
`base_artifact_id`, `policy`, `verification` and `manifest_sha256`. Resuming
validates that snapshot and source/base content; an incompatible selected executor
or CI plan requires a fresh generation. The external code-review context uses
version `2` for this selected protocol; existing plan/v1 contexts are preserved.

An intentional policy change requires a new review/verification generation.
The previous generation, check index and receipt remain auditable. A repair
that changes source similarly acquires a new artifact and obtains fresh review
authority. Per-event repair budgets and author ownership remain unchanged.

The [native review sequence](https://portal.thedevex.com/design/content-identity/native.svg)
shows first execution and resume. The
[external author sequence](https://portal.thedevex.com/design/content-identity/external.svg)
shows independent reviewer acquisition and exact-candidate CI.

## Native and external review

For native work, the trusted review/publication path selects the protocol,
freezes the content/policy snapshot and passes its exact identity with the
candidate. Review results retain their workflow and attempt bindings. Local
checks and Jenkins still execute the configured fixed commands and retain the
exact candidate's receipts. Publication never treats a content match as proof
that an unrelated Git commit passed CI.

For an external database author such as Muse, the trusted delivery provider
prepares a candidate in quarantine and acquires its source manifest. The native
reviewer independently acquires the same artifact from its selected checkout.
The content-v2 review validates the manifest identity and frozen policy,
alongside source assignment, context digest, workflow, review and lease. A
human-readable diff may inform review; its presentation is not the content-v2
artifact identity.

Jenkins submission, observation, exact-tree merge and post-merge release remain
separate authorities. Their existing candidate commit/tree, gate manifest,
correlation and authenticated transport bindings remain required. An approved
review alone is not a successful build, merge or portal rollout.

## Compatibility and limits

| Area | Current boundary |
| --- | --- |
| New selected work | Explicit `content-v2` uses canonical source and frozen policy/generation |
| Existing work | `legacy-v1` retains its original Git/diff/gate identities; no bulk rewriting |
| Review authorization | Still scoped to the exact workflow, review, source and admitted worker |
| CI and delivery | Still use the exact Git candidate and genuine immutable receipts |
| Live services | Source/configuration selection and qualified rollout are separate operations |
| Future adapters | Archive/object-store sources can implement the same manifest contract; not enabled here |
| Future transport | A fully non-Git CI/publication path needs its own admitted adapter and receipt contract |

Canonical full-index patch formatting remains the v1 compatibility fix for
producer/reviewer clone differences. Content-v2 removes patch presentation from
artifact equality without reinterpreting those historical review contexts.

## Acceptance and rejection checks

- Acquire identical content through at least two source implementations and
  distinct Git commits/checkouts; require equal artifact IDs. Change one byte,
  path or executable mode; require a different ID.
- Reject duplicate/unsafe paths, unsupported modes, links, oversized or changing
  sources, duplicate JSON fields and invalid digest/policy/generation envelopes.
- Freeze before checks or publication; fail persistence/admission and prove no
  check, push or Jenkins submission occurred.
- Resume from the persisted snapshot after runtime/config drift; preserve the
  selected policy and verification ID. Reject changed candidate content.
- Keep v1 receipts byte-compatible. Reject malformed v2 state without downgrade.
- Require fresh review/CI authority for another workflow or generation even
  when its artifact ID matches. Keep cancellation and per-event retries intact.

These are source acceptance criteria, not evidence of a live rollout. The
qualified implementation report records the actual tests run.

[External workflow](https://github.com/roachmaster/homelab/blob/main/docs/architecture/external-agent-workflow.md)
and [provider wiring](https://github.com/roachmaster/homelab/blob/main/.agents/skills/wire-forge-providers/SKILL.md)
describe the surrounding boundaries.

Implementation sources:
[pure identity values](https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/work_artifact_identity.py),
[source factories and adapters](https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/work_artifact_source.py),
[native snapshot provider](https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/work_native_identity.py),
and [external candidate provider](https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/work_external_identity.py).
