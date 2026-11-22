# Cargo cache publication correctness — 2026-11-22

The publication rollback fix passed 43 ordinary-UID regression methods and a separate **12/12 Root-owned filesystem component run**. The new source remains an **unapproved context** pending normal full Jenkins checks, independent Codex review and guarded merge. This report supplies correctness evidence; it supplies no new performance measurement or live activation proof.

The source is isolated from candidate `21a54876512724ef0ee4e98ce054a0d5f76f2d31`, with implementation commit `4ac1d50fa990465cfcdd7d772c6034a04955354e` and test-only follow-up `51c35693525390f1e2f773755630a232bc31c896`. The frozen supervisor SHA-256 is `04709f1e3aa948b5c35ba0423ce480186bba5e57207d97125e9ffd7602ddde68`. Full file and evidence hashes are in the [companion JSON](forge-cargo-cache-publication-correctness-20261122.json).

## The defect and corrected boundary

Genuine Codex review `87c6e680-8fa8-4cc5-9319-8b810c8e8723` requested revision for `cache-cancelled-publication-replaces-good`: cancellation after replacing `current.json` could fail verification while leaving the new pointer current and allowing the previous good snapshot to be collected.

The selected `CargoCachePort` now owns a verification-scoped transaction. It stages the new snapshot, retains the exclusive cache lock and exact old pointer/LRU bytes through logging and cleanup, and persists a pending journal before provisional metadata replacement. Durable success requires the exact passed, cleanup-confirmed result and committed marker. Cancellation is checked after the final marker write. Abort is persisted before restoration, and recovery resolves the journal before result replay or GC.

The previous target and unrelated victim snapshots survive aborts. Capacity includes the retained predecessor; optional publication skips when replacement cannot fit. Retention maintenance precedes the publication baseline and protects the selected main seed. Uncertain inventory is `null`, not a false empty cache. Deferred post-success cleanup does not downgrade a genuinely committed result.

## Ordinary-UID regressions

These two recorded runs cover **43 distinct methods**, rather than one combined 43-method invocation:

- Complete supervisor module: **42 passed**, zero failures/skips, **99.965 seconds**; helper `04709f1e…`, test source `5925ccc0…`.
- Additive committed-result I/O regression: **1 passed**, three subcases, **7.109 seconds**; unchanged helper, final test source `f6f7fe82…`.

Actual ordinary-UID child processes used `os._exit` at five boundaries: after pointer replacement; after the passed receipt before commit; after the committed marker; during abort after restoring the old pointer; and after predecessor deletion before journal removal. These tests bypass Python cleanup and verify fresh-provider recovery and same-action replay.

Other regressions cover late cancellation markers/signals, metadata and receipt/final-directory-fsync failures, cleanup/log-close/log-hash errors, unusable unrelated-cache fallback, unsafe saved-result replay, capacity, abandoned pre-journal snapshots, unrelated orphan GC, physical inventory and TTL. The old orphan/abandoned-scope regression was retained, with its crash barrier adapted to the new pre-journal rename boundary.

## Actual Root-owned filesystem proof

Run `d79349c3-ec97-44b1-9c4d-44a6207d5277` completed once: **12/12 passed**. The Root-private actor and helper were frozen mode0400 copies; fixture creation used a new exclusive private leaf.

- Actor SHA-256: `f8fd6e52e2c6572483650c5b4ff50ef5e9b7dbefb7357bc1390c8f065c38c0ce`.
- Raw Root report SHA-256: `1fd95651e80afb7514d3a2ececccf25bc1027fd2ab54f2dc5966d6a973707ee8`.
- Helper SHA-256: `04709f1e3aa948b5c35ba0423ce480186bba5e57207d97125e9ffd7602ddde68`.

| Group | Cases | Verified outcome |
| --- | --- | --- |
| Injected aborts | Before/after pointer, after LRU, after receipt, after marker, before result | 6 passed; exact old target/victim metadata and snapshot hashes retained; failed synthetic receipts |
| Simulated persistent-state recovery | After pointer, after receipt, during abort | 3 passed; rollback completed and no passed receipt remained for the aborted action |
| Simulated committed-state recovery | Committed marker, predecessor already removed | 2 passed; exact bound passed/cleanup-confirmed receipt retained |
| Commit, restore, capacity and TTL | One bounded synthetic artifact scenario | Passed; replacement restored, capacity skip preserved state, two expired entries removed |

This actor performed **filesystem operations only**. Its artifacts and receipts were synthetic. Signal handling was explicitly injected as a no-op; no real Root process death, supervisor execution, build-user cleanup, command, network, service or database operation was exercised. Recovery checkpoints captured owned state, allowed ordinary exception cleanup, restored the captured tree at the same fixture paths, and used a fresh provider. These are simulated persistent-state cases, separate from the real ordinary-UID process-death tests above.

## Exact AST comparison and changed code

Python 3.12.3 comparison used SHA-256 of UTF-8 `ast.dump(node, include_attributes=False)`, excluding source locations. The following **14 named functions** are identical to candidate21's historical b870 helper:

`derive_branch_scope`, `compute_compat_key`, `validate_publish_timestamps`, `validate_restore_timestamp_ordering`, `parse_cargo_duration_ms`, `parse_cargo_telemetry`, `_copy_tree_from_build`, `_copy_tree_to_build`, `run_child`, `main_ancestor_preflight`, `map_cargo_targets`, `_capture_source_inventory`, `_read_ui_qa_stages`, and `LocalCargoCacheProvider.resolve_key`.

The fixed `CHECKS` assignment is also identical, AST hash `b4bc47419c83dec917c770a0e1edd44d765ddcab96f4356917de5d86e6a00f96`. Per-function hashes are recorded in the JSON.

This is a selective syntax comparison. Eight existing nodes changed: `LocalCargoCacheProvider.__init__`, `_prune_locked`, `_read_pointer_locked`, `_reconcile_snapshots_locked`, `publish`, `restore`, plus `execute_locked` and `main`. New transaction/recovery functions were added. Entire publication, restore orchestration and supervisor behavior are therefore not claimed equivalent.

## Historical timing evidence remains historical

The original [four-phase report](forge-cargo-cache-qualification-20261121.md) and [machine-readable timing data](forge-cargo-cache-qualification-20261121.json) remain byte-for-byte unchanged:

- Markdown SHA-256: `5a7c318495e114cc9c406ced1b32332799bafa08f96ccafc3921564f92adc54d`.
- JSON SHA-256: `68e92dfb355ca2ae8ec807daebf9ab7102a4755d1ee3da2c283c28a9f3c90d01`.

They bind source `d4ea0741d2bf95fee8cb147323b117bcc4d44a00`, canonical helper `b870302a19504426efac6e46fda38a744138ec852951ae8d96a7d8b1b7b1607d` and rendered helper `ef7f917874e626fbcf97ff4c8826acc8c844d996bc45a3e72986f6168c47496a`.

| Historical b870 phase | Recorded total | Recorded Cargo completed-phase elapsed |
| --- | ---: | ---: |
| Cold | 2,188,383 ms | 882,550 ms |
| Warm | 1,285,286 ms | 98,580 ms |
| Clear/rebuild | 2,098,552 ms | 829,400 ms |
| Deliberate clear/cancel | 27,958 ms | Unavailable |

Cargo values are cumulative observed completed-phase elapsed time, not CPU-only compilation time. Those totals include the old publication/GC ordering. They are not measurements or a speedup claim for helper047, even where individual code routines remain identical.

Fresh full normal Jenkins checks and independent Codex review of the new candidate are still required. This addendum changes neither workflow budgets nor source-approval, merge or runtime-activation authority.
