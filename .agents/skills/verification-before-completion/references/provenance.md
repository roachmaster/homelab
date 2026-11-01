# Provenance — verification-before-completion

**Reconstruction dated 2026-11-01**, not restoration of the original staged bytes. The earlier draft and its source pins/checksums are unavailable. This adaptation uses the newly verified pinned sources below; it does not claim they were the original draft’s revisions.

- Upstream repository: [obra/superpowers](https://github.com/obra/superpowers)
- Pinned commit: `2ce8aa15d05bd57213e6f746f953aafb7ded7a8b`
- Local mode: repository-local and explicit-invocation-only; no global installation.
- License: [exact upstream MIT notice](../LICENSE), copied byte-for-byte including copyright and disclaimer.

## Verified upstream files

### `skills/verification-before-completion/SKILL.md`
- [Pinned file](https://github.com/obra/superpowers/blob/2ce8aa15d05bd57213e6f746f953aafb7ded7a8b/skills/verification-before-completion/SKILL.md) · [raw bytes](https://raw.githubusercontent.com/obra/superpowers/2ce8aa15d05bd57213e6f746f953aafb7ded7a8b/skills/verification-before-completion/SKILL.md)
- Bytes: `3646`
- SHA-256: `2befe7fc55bcadaa3d97dd9e8efeb633d2561c0ebe74c5a8b17c4d9e7e4520b3`
- Git blob SHA-1: `7d45333cc4a49c57a80df6c1fe2fa777a207afbc`

### `LICENSE`
- [Pinned file](https://github.com/obra/superpowers/blob/2ce8aa15d05bd57213e6f746f953aafb7ded7a8b/LICENSE) · [raw bytes](https://raw.githubusercontent.com/obra/superpowers/2ce8aa15d05bd57213e6f746f953aafb7ded7a8b/LICENSE)
- Bytes: `1070`
- SHA-256: `a37e0e9697144819e1d965176ac4ae5bc3fa02d11e7812036bbcadf6dafe2400`
- Git blob SHA-1: `abf0390320aa14406af7a520b9b0739fdda9bf08`

## Local adaptations

- Retained evidence-before-claims, claim-specific checks, final-state verification and delegated-artifact inspection.
- Replaced same-message-only and blanket all-success/all-delegation triggers with explicit invocation and evidence tied to the delivered artifact; meaningful scoped evidence is not mislabeled a full-suite result.
- Replaced blind revert-fix guidance with recorded pre-fix evidence or isolated controlled experiments that preserve user/concurrent changes.
- Added skipped/blocked coverage, docs-only checks, identity-verified reuse, and separate authorized publication/remote-CI verification.
- UI metadata is locally generated; `policy.allow_implicit_invocation: false` makes invocation explicit. No plugin suite, executable helper or global rules file is installed.
- The source and license hashes describe upstream bytes, not the adapted local `SKILL.md`.

Upstream wording is guidance, not authority to expand a user task or override repository rules. Existing Forge skills remain unchanged.
