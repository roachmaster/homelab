---
name: diagnosing-bugs
description: Diagnose a reported bug or performance regression using a symptom-specific reproduction, competing hypotheses and targeted evidence. Invoke explicitly for investigation or a scoped fix, not an unrelated refactor.
---

# Diagnose bugs

Use a feedback loop that can distinguish the user's actual failure from a nearby
symptom. Start by checking repository instructions, the affected contracts and
existing changes; read enough code to construct the right reproduction.

## Establish evidence

- Record expected versus observed behavior, input/configuration, relevant version
  and environment. Inspect available glossary/architecture decisions when they
  explain the affected boundary.
- Prefer an existing failing test or a small deterministic CLI/integration fixture.
  A successful exit alone is not a useful oracle for wrong output, duplicate
  effects or latency. Assert the specific symptom and relevant side effects.
- Run the reproduction and retain the command, exit status and useful redacted
  output. Narrow one input or dependency at a time while preserving that failure.
  For intermittent failures, report attempts and observed failure rate with the
  seed/timing conditions; do not convert a few passes into proof of a fix.
- If execution/access is unavailable, state what was and was not reproduced.
  Source inspection and provisional hypotheses can still help; do not claim a
  confirmed cause. Request only the missing access or redacted evidence needed.

## Discriminate causes, then fix

Rank a few plausible causes, each with a prediction and a probe that could disprove
it. The number is a judgment call, not a quota. Change one relevant variable per
probe. Prefer debugger state or narrowly targeted, tagged instrumentation over
logging entire requests. For slow code, measure a representative baseline and
compare equivalent workloads rather than guessing from code shape.

When history comparison or bisection is useful, preserve the working tree: use a
separate permitted worktree/copy or a reproducible fixture. Do not reset unrelated
work to obtain a clean experiment. Use isolated local resources; production probes,
replays with effects, load tests and instrumentation need applicable authorization.

If a fix is authorized, turn the reproduction into a regression at the seam that
actually reaches the bug. Verify failure before the fix where feasible, then verify
the fix and the original scenario. A shallow fake that omits the failing interaction
is not equivalent coverage. If no suitable seam exists, report that gap rather than
inventing a passing test. Keep the correction local; an investigation request alone
does not authorize source changes.

## Manual reproduction and sensitive evidence

No interactive shell template is bundled. When a human must reproduce the problem,
provide a short manual checklist: **setup/version; exact steps; expected result;
observed result; timestamp; relevant redacted lines or screenshot**. Tell them not
to send credentials, cookies, authorization headers or unrelated personal data.
Sanitize artifacts before quoting or saving them; avoid commands that print secrets.
Do not upload logs or enable instrumentation merely because this skill is invoked.

## Close the loop

Remove only temporary instrumentation/fixtures you introduced and no longer need;
retain the useful regression. Rerun relevant checks on the final state and review
the diff for unintended changes. Report **reproduction; supported cause; scoped
fix or recommendation; checks/results; remaining uncertainty**. Distinguish fake
coverage from real adapter behavior. Commit/push/install authority comes from the
current task or standing instructions, never from this skill.

Adapted under MIT; [pinned source, reconstruction and changes](references/provenance.md).
