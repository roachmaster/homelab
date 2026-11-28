# Native privileged Jenkins verifier

`forge-ci-check` and `forge-native-ci-check` are fixed public launchers. Each
accepts one canonical action UUID, or the side-effect-free `--describe-policy`.
Execution selects `/etc/forge-ci/pipeline.json` or
`/etc/forge-ci/native-pipeline.json` from the kernel executable identity. Public
launchers must be regular installed files at their exact `/usr/local/libexec/`
locations. Spoofed argv0, child flags and arbitrary config/command paths cannot
select execution authority.

The separate `forge-ci-build-child` executable belongs in the immutable policy
release directory, root:root0700, and is absent from sudoers. The public host
checks its independent SHA256 before every invocation. It receives a bounded,
root-owned child specification, creates an empty network namespace with loopback,
clears supplementary groups, permanently drops to the exclusive `forge_ci` UID
and GID, sets no-new-privileges and executes the fixed check. The dedicated build
UID must be distinct from the Jenkins Java agent and all credential owners.

Configuration version1/result_version4 selects the shared
`forge_jenkins::native_policy` descriptor and closed45/46 phase plans. Required
fields are `verification_profile`, `verifier_sha256`, `child_sha256`,
`policy_sha256`, `policy_path`, `projects`, `build_user`, `submitter_user`,
`spool_root`, `results_root`, `work_root`, `toolchain`, `cargo_registry`,
`node_modules` and `timeout_seconds`. Optional operator booleans are
`require_main_ancestor` (defaulttrue) and `portal_artifact_export` (defaultfalse).
`policy_path` is `/opt/forge-ci/{native-verifier-|verifier-}<binary-digest16>/policy.json`.
The descriptor is compared with the compiled policy and independently pinned.

The host captures the bounded submitter-owned request/bundle before untrusted
execution, checks complete tree bounds and source identity before checkout,
keeps source root-owned read-only, and exposes exact writable scratch links.
Execution has cleared environments, monotonic deadlines, bounded dual-stream
output, cancellation and process-group cleanup. Each check is followed by an
exclusive-UID termination/absence proof. Protected progress is written only by
the supervisor. Failed cleanup is recorded with `cleanup_confirmed:false`; no
passed receipt or reconciliation proof is fabricated.

Main portal export executes the actual isolated Cargo host build and `make
ui-check`, including deterministic assets/game payload/WASM checks. After UID
quiescence, it captures bounded regular bytes and the exact allowed Cargo host
hardlink alias, then atomically seals artifact-v1 into the root-owned results
root. Source/request identities, actual commands, hashes and sizes are preserved.
Replay checks every export path and byte.

Current checkpoint limitations: configured enabled Cargo caching is rejected
before effects, never silently downgraded. Explicit `cargo_cache:{"enabled":false}`
or absent cache is a real cold mode. Native cache transactions and stage-event
HTTP emission remain follow-up work. The protected progress channel and final
phase receipt are implemented. No live verifier selection, installed job,
privileged candidate run or whole branch/main execution has been performed by
these package tests. Local tests exercise actual unprivileged process and file
boundaries; the shared plan tests validate the full closed branch/main plans.
