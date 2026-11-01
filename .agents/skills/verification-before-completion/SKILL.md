---
name: verification-before-completion
description: Check whether completion, test, build, fix or publication claims are supported by current evidence. Invoke explicitly to review final work or prepare a truthful handoff; choose checks relevant to the actual change.
---

# Verify before completion

Tie each claim to evidence from the artifact being delivered. Confidence, an agent's
success message and an old green run are not substitutes for checking the current
result. This explicit-only skill does not make unrelated work a verification task.

## Claim → check → result

1. Identify the exact change and proposed claims. Read the diff and requirements;
   inspect relevant repository check instructions instead of inventing commands.
2. Choose the check that establishes each claim, then run it against the final
   applicable state. Read its full relevant output, exit status and failure/skip
   counts. Record the command and artifact/revision tested.
3. Compare the evidence with the claim. A formatter is not a build, a build is not
   a behavioral test, and a passing focused test is not a passing full suite.
   Static inspection and a test double do not prove a live adapter integration.
4. If source/configuration changes afterward, rerun the affected checks. If prior
   evidence is reused for byte-identical artifacts, verify that identity and state
   what was reused versus freshly executed; do not silently relabel an old run.
5. Report passed, failed, skipped and not-run/blocked checks separately. Explain
   the smallest unresolved prerequisite rather than weakening the check or claiming
   completion. A pending CI run remains pending.

## Match evidence to the promise

| Proposed claim | Minimum relevant evidence |
| --- | --- |
| Bug fixed | Original symptom-specific reproduction now passes; regression coverage or its gap identified |
| Regression detects the bug | Verified failing case before correction, or an isolated controlled counterexample—not a passing test alone |
| Tests pass | Named suite/profile completed on the tested state; failures and conditional skips disclosed |
| Requirements met | Each requested behavior mapped to implementation and meaningful proof; gaps retained |
| Delegated work is ready | Actual files/diff inspected and relevant checks verified, not just the worker's summary |
| Published successfully | Publication authorized, exact remote revision/content verified; associated required CI status reported separately |

For a red/green demonstration, prefer a recorded pre-fix failure or a separate
fixture/worktree. Do **not** blindly revert a fix, reset a branch, overwrite user
changes or edit expected assertions merely to manufacture a result. If a safe
counterexample is unavailable, say detection was not demonstrated.

Scope checks to risk and repository requirements: documentation/skill changes need
appropriate validation and links/review, not an invented claim that application
runtime tests ran. For code changes, preserve required full-suite/feature gates;
report any narrower interim result as such. Inspect scripts before running them
when their side effects are unknown. Use redacted evidence; additional credentials,
installs, deployments and external writes still require applicable authority.

Finish with **artifact/revision; commands and results; review/scope check; skipped
or blocked coverage; publication/CI state when relevant**. Avoid both unsupported
success claims and implying that meaningful partial verification proves nothing.

Adapted under MIT; [pinned source, reconstruction and changes](references/provenance.md).
