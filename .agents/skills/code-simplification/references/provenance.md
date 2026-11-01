# Provenance — code-simplification

**Reconstruction dated 2026-11-01**, not restoration of the original staged bytes. The earlier draft and its source pins/checksums are unavailable. This adaptation uses the newly verified pinned sources below; it does not claim they were the original draft’s revisions.

- Upstream repository: [addyosmani/agent-skills](https://github.com/addyosmani/agent-skills)
- Pinned commit: `8bfcc1f9916f2eea55494d8c1fa7f4bde867dd9e`
- Local mode: repository-local and explicit-invocation-only; no global installation.
- License: [exact upstream MIT notice](../LICENSE), copied byte-for-byte including copyright and disclaimer.

## Verified upstream files

### `skills/code-simplification/SKILL.md`
- [Pinned file](https://github.com/addyosmani/agent-skills/blob/8bfcc1f9916f2eea55494d8c1fa7f4bde867dd9e/skills/code-simplification/SKILL.md) · [raw bytes](https://raw.githubusercontent.com/addyosmani/agent-skills/8bfcc1f9916f2eea55494d8c1fa7f4bde867dd9e/skills/code-simplification/SKILL.md)
- Bytes: `13719`
- SHA-256: `e13b433345e6bf425c3625442ac1b3507b371c18d7ea3e3db822da7a7f742a29`
- Git blob SHA-1: `c4f794aa3fc63ea041989b30be7a3887c79c53ed`

### `LICENSE`
- [Pinned file](https://github.com/addyosmani/agent-skills/blob/8bfcc1f9916f2eea55494d8c1fa7f4bde867dd9e/LICENSE) · [raw bytes](https://raw.githubusercontent.com/addyosmani/agent-skills/8bfcc1f9916f2eea55494d8c1fa7f4bde867dd9e/LICENSE)
- Bytes: `1068`
- SHA-256: `6f202f8bd568cd730dbb2b0d1f8e243bc74c2fa1f64dbce9b2c7ea08bd5c9fd7`
- Git blob SHA-1: `d67778ada6b9cda6227e9130da182c13e73c8b2e`

## Local adaptations

- Condensed the principles and incremental workflow; preserved behavior, conventions, scope and reviewability as controlling criteria.
- Did not copy mechanical Map/async-wrapper transformations or numeric size thresholds as rules; called out their possible behavioral differences.
- Added Forge-specific preservation of typed ingress, Task/client separation, public config/factory/registrar integration, provider lifetimes and optional implementation dependencies.
- Replaced unconditional commit/revert language with task-authorized publication and scoped undo that preserves others’ work.
- UI metadata is locally generated; `policy.allow_implicit_invocation: false` makes invocation explicit. No plugin suite, executable helper or global rules file is installed.
- The source and license hashes describe upstream bytes, not the adapted local `SKILL.md`.

Upstream wording is guidance, not authority to expand a user task or override repository rules. Existing Forge skills remain unchanged.
