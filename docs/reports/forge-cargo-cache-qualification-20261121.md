# Forge cache operator qualification

Observed 2026-11-22T00:01:11.727828Z.

Qualification proof SHA256: `546015e03216d369dbddd72f49a8cc8bbb0ffb637d2cb51f4aa405628179435a`.

This report qualifies the corrected cache context commit `d4ea0741d2bf95fee8cb147323b117bcc4d44a00` on `refs/heads/ops/forge-cache-qualification-20261121`. These are isolated operator runs. They do not approve the task source, count as its new native CI, resume it, reset budgets, or authorize a merge.

| Phase | Result | Cache restore | Publish | Meaning |
| --- | --- | --- | --- | --- |
| cold | passed | miss | published | All three fixed gates passed |
| warm | passed | hit_branch | published | All three fixed gates passed |
| clear-rebuild | passed | miss | published | All three fixed gates passed |
| clear-cancel | failed | miss | skipped_cancelled | Intentional cancellation; not a passed build |

The two actual scoped clear invocations are separately preserved with argv, time, scope, input inventory SHA and output SHA. Cold, warm, clear rebuild and cancellation share the exact helper, source/tree/main/gates/branch and compatibility key.

Preserved source snapshot: task `0b3828b5-b79b-46dd-9d1a-76444a52454b`, workflow `6339494b-4293-4230-b49f-57cca66bd64a`, attempt `171a9198-f942-425e-abc2-d075794ae5fa`, review `8222a349-998d-438b-8938-5650e2808a84`, revision6. At this recorded snapshot, the genuine independent code decision was **REVISE**, with three findings; the workflow was escalated. Its recorded repair/stagnation counters were **2/3** and **2/2**. This export rewrote no source or receipt.

The correction ref is context for the author to inspect only after a real new claim. The one explicit owner continuation, if later authorized and executed, must still use a fresh genuine reconciliation of the unchanged native workspace, followed by new fixed checks and independent Codex review. Any further automatic repair is fenced; successful review and exact-head verification still precede trusted merge.

The companion JSON contains only selected identifiers, hashes, statuses and counts. Raw logs, credentials, prompts, model outputs and private filesystem paths remain outside the documentation export.

## Measurement context

Observed on Mission Control: eight guest vCPUs across four SMT cores, with pairs `(0,1)`, `(2,3)`, `(4,5)`, `(6,7)`, and Linux `6.8.0-147` on x86_64. Other work ran concurrently; these are observed qualification timings, not an isolated or deterministic benchmark. The cached and cleared runs used identical source, helper, gate definitions and compatibility key.

The cold and warm runs completed all fixed gates, including 785 backend tests each. The explicit post-clear rebuild also passed every gate. The fourth action was deliberately cancelled through Jenkins after actual fresh compilation; it correctly failed, skipped publication and confirmed cleanup. It is not a passing CI run.

Production native cache activation remains pending the actual task's independent source review and trusted merge. A measured snapshot is about 14 GB; the proposed production budget is 64 GiB with eight entries maximum and fourteen-day retention. The isolated qualification configuration stayed at 20 GiB throughout these four actions.

| Observed metric | Cold | Warm | After clear |
| --- | ---: | ---: | ---: |
| Total | 36m 28s | 21m 25s | 34m 59s |
| `make ci` | 20m 25s | 5m 30s | 19m 40s |
| `make ui-qa` | 3m 33s | 2m 31s | 3m 26s |
| Backend gate | 9m 27s | 9m 22s | 9m 31s |
| Cargo compilation units across build and browser gates | 390 | 8 | 390 |
| Backend tests | 785 | 785 | 785 |

The observed warm total was 41.3% shorter than the cold total. Cache restore took 79.1 seconds; the tests and browser journeys still ran. The cancellation action is excluded from this performance comparison.

At qualification time Mission Control already runs the reviewed owner-continuation API source `1623ba5e7b24e98db03c832dc842a654bc72704e` and schema26. Its strict live verification, existing-access preservation and full owned backup/restore passed. That deployment is separate from this cache context: preserve shipped migration26, current main, native policies and old receipts when applying the cache corrections.
