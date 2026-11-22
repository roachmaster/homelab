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

## Retained compilation and stage measurements

This addendum corrects the original export omission using the **same four retained runs**. No new build or measurement was made. The approved Plan (SHA`da4dea08…`, section6.2) explicitly requires cold/warm total time, compilation time and individual stage durations.

All values below are measured milliseconds (`estimated=false`), copied from protected Root receipts and checked against their hash-bound raw fixed-check log spans. `cargo_compile_ms` sums Cargo’s own `Finished … in …` elapsed phase durations inside completed build/browser gates. It is **not compiler CPU time**, crate-count-derived time, or a separately additive wall-clock stage. The32 build-gate and4 browser-gate Cargo completions per full run can include already-fresh invocations. Compiler CPU time was not recorded.

| Metric (ms) | Cold | Warm | Clear/rebuild | Clear/cancel |
| --- | ---: | ---: | ---: | ---: |
| Supervisor total | 2188383 | 1285286 | 2098552 | 27958 |
| Cargo reported compilation phases (check0+1) | 882550 | 98580 | 829400 | Unavailable |
| `prepare_source` | 982 | 1087 | 1073 | 935 |
| `cache_restore` | 0 | 79083 | 0 | 0 |
| `check_0` | 1224790 | 329565 | 1179907 | Unavailable / not completed |
| `check_1` | 213498 | 151097 | 205662 | Unavailable / not completed |
| `check_2` | 566857 | 562397 | 571206 | Unavailable / not completed |
| `verify_source_clean` | 119 | 109 | 115 | Unavailable / not completed |
| `cache_publish` | 175736 | 155926 | 134905 | Unavailable / not completed |
| `cleanup` | 5988 | 5560 | 5124 | 411 |
| Cargo check0 compilation phases | 818580 | 95330 | 768820 | Unavailable |
| Cargo check1 compilation phases | 63970 | 3250 | 60580 | Unavailable |

The intentionally cancelled action has no completed fixed-check stage. Its compilation duration and compilation-unit total are **null/unavailable**, not zero; the interrupted interval cannot be reconstructed as compilation time. Its recorded prepare, restore and cleanup intervals remain shown. Stage durations are separate recorded intervals, not an exhaustive CPU profile; do not sum nested Cargo or browser timings into supervisor wall time.

| UI-QA substage (ms; inside check1) | Cold | Warm | Clear/rebuild |
| --- | ---: | ---: | ---: |
| `prerequisites` | 0 | 1 | 0 |
| `generated-assets` | 4247 | 4441 | 4234 |
| `agent-and-fixture` | 63850 | 3110 | 60466 |
| `browser-journeys` | 144619 | 142690 | 140119 |

### Exact measurement provenance

Source`d4ea0741d2bf95fee8cb147323b117bcc4d44a00`, tree`8728c992c129c7e6ff899c814a2651867e40b71d`, expected-main`a08b71862178e4026ca407e2258bb60ca7eac39b`. Canonical helper`b870302a19504426efac6e46fda38a744138ec852951ae8d96a7d8b1b7b1607d`; qualification-rendered helper`ef7f917874e626fbcf97ff4c8826acc8c844d996bc45a3e72986f6168c47496a` (only its fixed configuration path differs). Job`forge-cache-qualification`; compatibility key and gate remain those in the original qualification proof.

| Phase | Action UUID | Protected receipt SHA256 | Raw log SHA256 |
| --- | --- | --- | --- |
| cold | `ff48079b-77d8-5dc1-8e26-c74475645197` | `af69160850ed7bf1280f6af5ef6b377878788be68bf8652eff6ee7f837968165` | `94934b0ccacaf841a7227eb27b25ae3386ebb6d6c7f11f3425b86fb9edaa7f18` |
| warm | `291d75a1-e961-50b0-b183-6b474fe88eef` | `21fe7be56a88c561962a2038a7b18100286a08969391cf45519460a322cb3f37` | `6c607367c63b8c7a30bcb9089b63f7fddd5205c045ba72671b2c2aac5f20e79c` |
| clear-rebuild | `1a451a27-30a0-5230-b511-83ff88addb44` | `d56f192c1c21428a4405ced5812a11320df0fbc9a7f61c4a84a3d7d52a2d4866` | `00ef2b7e83eddbc987a6a7165b8e8259cecb3ec69cbe176e0294ce0e7460cd8a` |
| clear-cancel | `e0f6ba4b-72fc-53f6-bfef-8a8162445618` | `fa2c23bae814be13b7a8244a783bc6d7034028fe4368fd3813fb5f8597080be7` | `5afcb8b94bff57145dcf70f48f916c018de7619c35ddb79f8a6adbfefedf95d0` |

The companion JSON retains exact per-phase workflow/correlation/source/helper identities, every available supervisor stage, UI substage durations, Cargo completion counts and units, and explicit unavailable-value reasons. Original receipts, logs, proofs and reports remain preserved. This correction supplies evidence only; it creates no source approval, task admission, budget change, CI result or deployment.
