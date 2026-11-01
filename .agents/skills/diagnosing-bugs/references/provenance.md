# Provenance — diagnosing-bugs

**Reconstruction dated 2026-11-01**, not restoration of the original staged bytes. The earlier draft and its source pins/checksums are unavailable. This adaptation uses the newly verified pinned sources below; it does not claim they were the original draft’s revisions.

- Upstream repository: [mattpocock/skills](https://github.com/mattpocock/skills)
- Pinned commit: `4ae2317ddb22b046e26455e4f9c9b3aaaf62212d`
- Local mode: repository-local and explicit-invocation-only; no global installation.
- License: [exact upstream MIT notice](../LICENSE), copied byte-for-byte including copyright and disclaimer.

## Verified upstream files

### `skills/engineering/diagnosing-bugs/SKILL.md`
- [Pinned file](https://github.com/mattpocock/skills/blob/4ae2317ddb22b046e26455e4f9c9b3aaaf62212d/skills/engineering/diagnosing-bugs/SKILL.md) · [raw bytes](https://raw.githubusercontent.com/mattpocock/skills/4ae2317ddb22b046e26455e4f9c9b3aaaf62212d/skills/engineering/diagnosing-bugs/SKILL.md)
- Bytes: `8664`
- SHA-256: `849198df88cdb65fd0d97236604c1fca7ff2f22d2f705f9724a800d0266b118d`
- Git blob SHA-1: `d039541ddd1c46e6ddbd662fe99d49b686fc5e12`

### `LICENSE`
- [Pinned file](https://github.com/mattpocock/skills/blob/4ae2317ddb22b046e26455e4f9c9b3aaaf62212d/LICENSE) · [raw bytes](https://raw.githubusercontent.com/mattpocock/skills/4ae2317ddb22b046e26455e4f9c9b3aaaf62212d/LICENSE)
- Bytes: `1068`
- SHA-256: `0e7ac423bf2c6e223b7c5b156f8cf72da49d748e56a1641402c31f22ad07dbb5`
- Git blob SHA-1: `f1dd2c09108dde1a5f56097cee8461b3ea834499`

## Local adaptations

- Condensed the six-phase workflow while retaining symptom-specific feedback, minimization, falsifiable hypotheses, targeted probes and regression evidence.
- Replaced mandatory interactive HITL shell-template references with a manual redacted reproduction checklist; no shell helper, network replay or instrumentation is automatically authorized.
- Allowed bounded source inspection and clearly provisional hypotheses when a runnable reproduction is unavailable, rather than inventing evidence or blocking all investigation.
- Removed fixed hypothesis/stress-loop quotas and aggressive unbounded retry language; added preservation of unrelated work and explicit investigation-versus-fix scope.
- UI metadata is locally generated; `policy.allow_implicit_invocation: false` makes invocation explicit. No plugin suite, executable helper or global rules file is installed.
- The source and license hashes describe upstream bytes, not the adapted local `SKILL.md`.

Upstream wording is guidance, not authority to expand a user task or override repository rules. Existing Forge skills remain unchanged.
