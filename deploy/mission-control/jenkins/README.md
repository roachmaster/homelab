# Headless Jenkins on Mission Control

Follow the [persistent agent branch and cache rules](../../../AGENTS.md) for every profile
and configured alias: one permanent `feature/<agent>`, no agent-created per-task
branches, rebase onto current main before authorized pushes, incremental pushes
after green tests, and agent-managed local caches. Native workers cannot commit
or push; Plan/review remain read-only. Older `forge/agent/*` defaults below are
compatibility bindings requiring receipt-preserving reconciliation before author
work. The named **legacy machinery-owned publication exception** retains
`forge/task-<publication-id>` in `work_publication_clients.py:branch_name()` for
explicit ephemeral policy, legacy identities or missing persistent resolution.
Preserve frozen receipts; this documentation grants no new permissions.

## Native deployment candidate

The v4 deployment source builds both privileged Jenkins verifier entry points and
their private build helper from the `forge-ci-verifier` crate. `forge-jenkins-install`
provisions Jenkins and Java. `forge-jenkins-setup-ci` installs the executor, jobs,
and verified binaries. Its `--native-only --execute` mode provisions the
`forge-native-candidate` job. Provisioning uses a root-owned release directory
with the companion Groovy files, `forge-ci-check`, `forge-native-ci-check`,
`forge-ci-build-child`, and `forge-ci-cache-clear`. The installer obtains the
descriptor from the binary and compares it with the contract compiled into the
installer before selecting a release. The offline Cargo registry must contain
the exact packages in the candidate lock file.

| Verified scope | First stage | Second stage | Third stage |
|---|---|---|---|
| Agent branch or detached | source preflight | checks | in-memory backend |
| Main | verified checks | real browser journeys | backend suites with disposable PostgreSQL |

The numbered phase plan expands these stages to 45 branch or 46 main checks.
The v4 backend commands are `make backend-suites SCOPE=branch` and
`make backend-suites SCOPE=main`. Profiles v1–v3 remain available to validate
prior receipts. Records below describe their installation and measurements.
The [native verifier](../../../src/rust/forge-ci-verifier/README.md) and
[agent work log](../../../AGENT_WORK.md) record the qualification and rollout state.

## Earlier Jenkins profiles and operating records

### Package updates and planned agent restarts

Ubuntu 24.04's installed `needrestart` APT hook automatically restarts affected
services after package transactions. Such a restart stops every process in
`forge-jenkins-ci-agent.service`, including a running verification build.
`setup-ci.py` installs a root-owned 0644 fragment at
`/etc/needrestart/conf.d/99-forge-ci-agent.conf` that defers automatic restart
of this exact service. The agent remains visible as requiring a deferred
restart. Package installation and all other service restart policies remain
unchanged. See the installed `/usr/share/doc/needrestart/README.Ubuntu` and
`/etc/needrestart/needrestart.conf` for the packaged behavior and `override_rc`.

From the reviewed native provisioning directory, use the restart-deferral mode:

```sh
sudo ./forge-jenkins-setup-ci \
  --agent-restart-deferral-only --execute
```

This mode needs no controller credentials and performs no service restart or
job submission. Full and native-only provisioning also install this policy.
To apply pending security updates to the running agent, temporarily mark
`forge-mission-control-ci` offline in Jenkins, wait for its executor and owned
verification processes to finish and clean up, then restart only
`forge-jenkins-ci-agent.service`. Verify its authenticated reconnection to the
same node before returning the node online. Preserve any interrupted build's
real failure and cleanup receipt; a new successful verification must come from
the normal exact-source CI path.

The exclusive `forge-mission-control-ci` node has one executor and label
`forge-ci`; controller executors remain zero. `forge-jenkins-ci-agent.service`
connects through WebSocket. `forge-exact-candidate` invokes the root-owned
`/usr/local/libexec/forge-ci-check` wrapper with seven validated correlation and
source parameters. The last installed native verifier is SHA256
`8d0cdefcb034d486841587e6d5b4eb504c8e727730b9694a2a1fa0fa4e5c64c1`,
profile `forge-sdlc-jenkins-v1`. It runs `make ci`, `make ui-qa` and the
agent-runner Python suite on every scope. Source v1 preserves those exact
commands and does not force database skips on branches. It does not run the
other three backend suites automatically.

The source-only v2 contract below fixes scope accounting and complete main
coverage. It has not replaced the installed helper/configuration. The release
at `95651a5` passed native #17 plus the separate operator companion #1: 954
agent-runner tests and services 6 / driver 2 / studio 3, all with zero skips. That
one-off companion is release evidence, not a permanent replacement for the
standard v2 main gate. Candidate commands remain unprivileged `forge_ci`
processes with private network, read-only source and owned outputs. Jenkins
and model credentials stay outside that boundary.

The configured wall deadline is 2,700 seconds after a measured cold smoke build
took 29m 29s. Preserve that production value when preparing future configuration;
`setup-ci.py` now defaults to 2,700 seconds (the original installer default was
1,800 seconds). Persistent local compiled Cargo artifact caching is implemented
below under `/mnt/data/forge-homelab/ci-cache` (activated on Mission Control
when the updated `setup-ci.py` and verifiers are deployed). Do not describe the
existing registry/toolchain/browser caches as a compiled artifact cache.

External Muse delivery uses the typed Rust Jenkins client through the trusted
delivery worker. Native Gemini uses the publisher's explicit `jenkins` provider
selection, `native_delivery_mode: "branch-first-v1"`, and private
`jenkins_delivery_config`. Its separate `forge-native-candidate` job is installed
with `setup-ci.py --native-only --execute` and the existing private client
configuration. This preserves the original job/verifier for frozen external
work. The native job checks current-main ancestry before compilation; a stale
branch fails as a real build and returns to the agent with its build reference.
See the
[native workflow sequence and manifest contract](../../../docs/architecture/forge-jenkins.md#native-gemini-workflow).
This changes no historical workflow recipe or GitHub Actions setting. Submission
acceptance is not a successful build: guarded merge requires real full-suite
success, exact source receipts, independent review and confirmed process cleanup.

Production spool and result paths are configured privately. Give only the
trusted publisher the dedicated spool/state write paths and its local
`SupplementaryGroups=jenkins` access; do not add the operator globally to the
Jenkins group. Reviewers and native model processes receive neither credentials
nor publication authority. Restart cleanup uses the publisher's durable action
journals and scoped database status, preserving uncertain actions without
duplicate submission.

The service listens only on `127.0.0.1:18084`. Its persisted home is
`/mnt/data/forge-homelab/jenkins`. The installer masks package auto-start until
the secure bootstrap is ready, creates `forge-admin` through a systemd-delivered
credential, disables signup/anonymous access/inbound TCP agents, and leaves
zero build executors on the controller. Default CSRF protection remains enabled.
The base installer adds no plugins or GitHub credentials. The separately
authorized GitHub access step below configures the Unix service account. Existing Jenkins
data is rejected instead of overwritten. Failed setup is retained for inspection,
without an automatic retry. Successful reruns verify the saved installation.

Inspect the job with `systemctl status forge-jenkins-install-20261120.service`
and `systemctl status forge-jenkins-verify-20261120.service`, plus
`/var/lib/forge-provisioning/jenkins/{status,complete}.json`. Verify readiness
through the authenticated API and a real restart. For remote browser access:

```sh
ssh -L 18084:127.0.0.1:18084 lrocha@mission-control
```

Open `http://localhost:18084`. Retrieve the generated password locally with
`sudo cat /etc/jenkins/credentials/admin-password`. Never commit it or include it
in job logs. No public reverse-proxy route is created by this installation.

## Verified installation

Jenkins **2.580.2** completed setup on 2026-11-20 at 23:59:39 UTC. The native
service is enabled at boot. Authenticated API requests succeeded, anonymous API
requests were denied, signup and inbound agent TCP were disabled, and CSRF
protection was active. The same checks passed after a real restart. Java may
report its loopback listener as `[::ffff:127.0.0.1]:18084`; this is the IPv4-mapped
form of the same loopback address. Wildcard and other listeners fail verification.

The initial install job retained two failed verification/provisioning attempts:
the job's restrictive umask initially hid the signing key from APT, then the
listener check rejected the equivalent IPv4-mapped address. Both were corrected
without weakening signature or listener checks. Private evidence remains in
`/var/lib/forge-provisioning/jenkins/attempt-1` and `attempt-2`. The final
`forge-jenkins-verify-20261120.service` job succeeded. Its explicit
`--verify-existing` continuation reused this job's saved bootstrap and credential,
and performed the health and restart checks without reinstalling or recreating
accounts. The final root-owned installer SHA-256 is
`3b1f026d3d853a3bcc3739f990f8910fb40610f80219d4640974b7af975cc0da`.

## Source profile v2: explicit rollout contract

The pure `work_ci_contract.py` factory selects a frozen plan at bootstrap.
The standalone root verifier contains the same closed declarations and never
imports candidate code for privileged policy. New `setup-ci.py` installations
render `forge-sdlc-jenkins-v2`; old configuration files remain v1 by default.
Native-only setup inherits the existing profile unless the operator explicitly
selects `--verification-profile forge-sdlc-jenkins-v2`. No setup command is
executed as part of this source change.

V2 runs three real argv vectors per scope:

| Scope | Stage 1 | Stage 2 | Stage 3 |
|---|---|---|---|
| Branch/detached | `/usr/bin/make ci-preflight` | `/usr/bin/make ci` | `/usr/bin/python3 -B deploy/mission-control/backend-suites.py --scope branch` |
| Main | `/usr/bin/make ci-verified` | `/usr/bin/make ui-qa` | `/usr/bin/python3 -B deploy/mission-control/backend-suites.py --scope main` |

`ci-verified` runs the cheap qualification preflight before the Rust matrix.
Main then runs real browser QA and the complete services, agent-driver,
agent-runner and studio suites with zero skipped tests. Branch mode states
browser/database `not_run`; it accepts only the declared PostgreSQL exclusions,
rejecting other skips and empty execution. Its hosted names are
`source-preflight`, `check`, `in-memory-backend`. Main names are `check`,
`real-browser`, `backend-suites`. No omitted browser test is marked green.

A v2 publication configuration must explicitly supply its profile, scope,
exact checks and hosted tuples. For example, branch configuration adds:

```json
{
  "jenkins_verification_profile": "forge-sdlc-jenkins-v2",
  "jenkins_verification_scope": "branch",
  "checks": [
    ["/usr/bin/make", "ci-preflight"],
    ["/usr/bin/make", "ci"],
    ["/usr/bin/python3", "-B", "deploy/mission-control/backend-suites.py", "--scope", "branch"]
  ],
  "required_hosted_checks": [
    ["jenkins/branch", "Forge branch CI v2", "source-preflight"],
    ["jenkins/branch", "Forge branch CI v2", "check"],
    ["jenkins/branch", "Forge branch CI v2", "in-memory-backend"]
  ]
}
```

The existing delivery config separately selects
`"verification_profile": "forge-sdlc-jenkins-v2"`. The new workflow repair
policy must freeze the same logical names in `required_hosted_checks`; an old
workflow is not upgraded implicitly. Main selects scope `main` and its exact
plan above. A main proof requires the candidate to equal observed main, so
pre-merge branches cannot request main coverage under another label.

V1 request/result identities and historical receipts remain strict and
readable. V2 versions its request, result, native binding, scope and coverage;
producer, delivery and native consumers all require the same exact records.
Its new policy/runner paths are protected source inputs. Both profiles also
protect the newly introduced shared contract; historical trees without that
file retain the exact old v1 source map. Main and branch
receipts cannot be substituted for one another, even when both have three
rows. No SQL migration or historical evidence rewrite is involved.

See [the pipeline design](../../../docs/architecture/ci-pipeline.md#7-versioned-execution-and-accounting).
A coordinated rollout must install matching worker/profile/verifier
configuration. The existing publication coordinator now has an optional
post-merge verification owner: migration28 captures confirmed native and external
merges transactionally, and the selected worker submits the actual merged commit
through the existing Rust-backed Jenkins provider. This adds no new service or
broker. It is implemented in source and has not been activated on Mission Control.

Publisher configuration explicitly selects both `post_merge_enabled: true` and
an absolute `post_merge_jenkins_delivery_config` path. That separate private
delivery configuration must select v2, matching repositories, credentials scope,
job and verifier. Provision migration28 and its strict catalog/privilege checks,
then an enabled owner/project worker grant before starting this configuration.
Legacy configuration remains disabled, and historical completed merges are not
backfilled. Failed verification writes independent remediation evidence; it
neither reopens the completed task nor bypasses normal policy to admit model work.
See [the coordinator ownership and sequence design](../../../docs/architecture/forge-post-merge-verification.md).


## GitHub access for the Jenkins user

On 2026-11-21 the operator explicitly authorized sharing the existing main
account's GitHub credentials with the `jenkins` Unix user. The reviewed
`connect-github.py` job copied only the active `github.com` account and selected
SSH identity into `/var/lib/jenkins`, its actual Unix home. The Jenkins data
directory is a separate path. Credential files are Jenkins-owned mode0600 inside
mode0700 directories. No secret contents are in this repository or job output.

The GitHub-specific SSH configuration requires the selected key, noninteractive
authentication and strict verification against the operator's existing GitHub
host-key pins. The existing token authenticates GitHub CLI; HTTPS Git uses
`gh auth setup-git --hostname github.com`. All target writes and permission
changes execute in a child permanently switched to Jenkins UID/GID with empty
supplementary groups. Root only reads the authorized source credentials and
writes the protected provisioning report. No sudo/group membership is added.

Local job `forge-jenkins-github-access-20261121.service` succeeded at00:11:24UTC.
GitHub API identity and SSH authentication both identify `roachmaster`. GitHub
reports read/write access to `roachmaster/homelab`; HTTPS and SSH `git ls-remote`
both returned main `a3968c17841bddc533fa76f1df491542930a9987`. These were read-only
checks; no push, build or GitHub workflow was submitted. Private evidence is
`/var/lib/forge-provisioning/jenkins/github-access.json` and its adjacent intent.

To use the configured account from an operator shell:

```sh
sudo -H -u jenkins sh -c 'cd "$HOME" && gh api user --jq .login'
sudo -H -u jenkins sh -c 'cd "$HOME" && git ls-remote git@github.com:roachmaster/homelab.git refs/heads/main'
```

This configures service-user Git and CLI authentication. Jenkins credential-store
entries and the CI pipeline were separate follow-up work; the pipeline is now
installed as described above. The stopped Grok workflow remains stopped.

GitHub CLI reference: [credential-helper setup](https://cli.github.com/manual/gh_auth_setup-git).

## Mission Control Rust client

The [`forge-jenkins` capability](../../../src/rust/forge-jenkins/README.md) now
provides typed Jenkins I/O through exported factories and registrars. The
[Forge CLI integration](../../../docs/architecture/forge-jenkins.md) adds bounded
explicitly selected Jenkins commands and retains existing admission and provider
boundaries. Its installed local command can read the controller:

```sh
forge --jenkins-config "$HOME/.config/forge/jenkins/mission-control.json" jenkins.status
forge --jenkins-config "$HOME/.config/forge/jenkins/mission-control.json" jenkins.jobs
```

`provision-client-token.py` created the named **Mission Control - Forge CLI** token
through Jenkins's native API, with a180-day expiration (2027-05-20). It used the
existing administrator password only for the provisioning request, retaining the
CSRF crumb's session cookie. Runtime calls use Basic username/API-token auth,
never that password. The token inherits the existing administrator account's
permissions. Client mutation policy defaults tofalse; it is not a narrower
server-side role. Configuration and token files are private and outside Git.

The reviewed provisioner durably saves its intent before issuing the token, saves
a protected secret receipt for delivery recovery, and refuses to repeat an
uncertain creation. Operator-home writes run after permanently dropping UID/GID;
token/intent values never enter command arguments or logs. Successful reruns
reuse and verify the same receipt instead of creating another token.

Initial client acceptance verified status, the then-empty job list, explicit
mutation denial and a typed absent-queue response. Later pipeline acceptance ran
a real full-suite Jenkins smoke build; its source and results are recorded in
`AGENT_WORK.md`. That smoke result is not a CI receipt for a current task. The
native service's security settings and stopped Grok run remain as recorded above.

## Persistent local Cargo build caching

`verify-candidate.py` (installed to `/usr/local/libexec/forge-ci-check` for
`forge-exact-candidate` and `/usr/local/libexec/forge-native-ci-check` for
`forge-native-candidate`) manages persistent local Cargo build caching under
`/mnt/data/forge-homelab/ci-cache` without changing any CI gate, adding external
artifact servers, or granting candidate code additional privileges or network
access.

### Provider wiring and isolation boundaries

- **Typed configuration (`CargoCacheConfig`)** is parsed and validated effect-free
  at bootstrap from `/etc/forge-ci/pipeline.json` (and inherited into
  `/etc/forge-ci/native-pipeline.json` by `setup-ci.py --native-only`). `root`
  must be an absolute, non-symlink path strictly under `/mnt/data/forge-homelab`.
- **Substitutable port (`CargoCachePort`)**: `CargoCacheFactory` freezes
  `CargoCacheConfig` before any disk or process effect and selects either
  `LocalCargoCacheProvider` (when `enabled: true`) or `NullCargoCacheProvider`
  (zero-effect fallback when disabled or substituted in tests). The selected port
  is injected into `execute(action, config, cache_port=...)`.
- **Canonical execution slot**: Serialized under `results_root/executor.lock`,
  each verification runs in `<work_root>/active/{source,scratch/{target,package-targets,cargo-home,home,qa}}`.
  Stable `CARGO_MANIFEST_DIR`, `CARGO_TARGET_DIR`, `OUT_DIR`, and `CARGO_HOME`
  paths let Cargo reuse compiled artifacts accurately. `<work_root>/active` is
  wiped before every run and again in `finally` after `kill_build_user(account)`.
- **Privilege isolation**: `/mnt/data/forge-homelab/ci-cache` is `0700 root:root`.
  The unprivileged `forge_ci` build account never sees or accesses the cache root.
  Publication runs only after `kill_build_user(account)` confirms zero surviving
  `forge_ci` processes, all three fixed checks exit `0`, and `git status` is clean.
  Copy-out from `forge_ci` scratch uses `O_NOFOLLOW` + `fstat` on every component
  and rejects symlinks, FIFOs, sockets, device nodes, or non-`forge_ci` files
  (`publish_status: "skipped_unsafe_artifact"`), preserving any prior known-good
  snapshot.

### Trusted source-branch scoping

Branch cache identities are derived exclusively by trusted delivery components
(`work_external_delivery.py:TrustedDeliveryPort.prepare_candidate`,
`work_native_jenkins.py:NativeJenkinsVerification._prepare`, and
`TrustedDeliveryPort._submission`), propagated immutably through submitter-owned
`request.json` (without changing Jenkins job parameters in `ci-node.groovy` or
`native-ci-job.groovy`), and verified fail-closed by `verify-candidate.py` before
any cache or filesystem access:

- **`main`**: Allowed **only** when `candidate_commit == expected_main` (exact
  `main` verification). Writes to `scopes/<project>/main/<compat_key>`.
- **`branch/<slug>`**: Resolved for external delivery from trusted remote
  `refs/heads/*` via `git ls-remote --heads` (ignoring ephemeral
  `refs/heads/forge/external-candidate-*` refs), or for native branch-first
  delivery from the publisher's verified pushed branch receipt (`forge/task-*`).
  An optional external author manifest `source_branch` is accepted only when
  independently verified against the remote branch tip or verified ancestry
  (`merge-base --is-ancestor`); spoofing another branch or claiming `main` fails
  closed before submission. Slugs use `<sanitized-prefix>-<sha256(branch)[:16]>`
  to prevent path traversal and collisions (`feat/a-b` vs `feat-a/b`). Writes only
  to `scopes/<project>/branch--<slug>/<compat_key>`.
- **`detached`**: Selected when zero or multiple remote heads match an explicit-free
  candidate (or non-`main` legacy integrate-first runs without a published branch).
  Strictly **non-writable** (`publish_status: "skipped_unscoped"`).
- **Read-only `main` seeding**: When `seed_from_main: true`, a cold `branch/<slug>`
  or `detached` run may copy `main`'s matching `<compat_key>` snapshot as a
  read-only seed (`restore_status: "hit_main_seed"`) without ever modifying `main`.
  If a branch's own snapshot exists and fails integrity or timestamp ordering,
  it falls back directly to a clean build rather than silently switching snapshots.
- **Sibling-scope seeding**: When `seed_from_siblings: true`, a cold
  `branch/<slug>` run with no entry of its own first tries the most recently
  published compatible entry from another branch scope
  (`restore_status: "hit_sibling_seed"`), falling back to the `main` seed.
  Retries on a new branch scope usually share the retry's source state, so a
  sibling snapshot matches more fingerprints than `main`'s older one. Like the
  `main` seed, the sibling entry is read-only during the restore and the
  compat-key match is still required — an incompatible entry is never reused.
  Detached runs do not seed from siblings.

The sibling-seed and recursive `incremental`/`doc`/`ast` exclusions described
here are source features pending a qualified helper rollout. The installed `8d…`
native verifier has neither feature. Its last measured run was cold: 16m59s
Rust, 2m54s browser and 13m00s backend, followed by 1m47s to publish 33,814 files
(12.83 GB). Failed attempts published no snapshot. No warm-run speedup from
that new snapshot has yet been measured.

### Compatibility key (`compat_key`)

`compat_key` is the SHA-256 of canonical JSON covering:

- `cache_schema_version` (`1`), `project_alias`, `verification_profile`, and `CHECKS`
- Toolchain fingerprint: `rustc -vV`, `cargo -V`, SHA-256 of `rustc`, `cargo`,
  optional `clippy-driver`, and `lib/rustlib/{components,manifest-*}`
- Host platform (`sys.platform`, `platform.machine()`)
- Effective compiler/profile env (`RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`,
  `RUSTDOCFLAGS`, `CARGO_INCREMENTAL`, `CARGO_PROFILE_*`, `CARGO_BUILD_*`, `CC`, `CFLAGS`)
- Git blob SHAs of repo Cargo/toolchain config files (`.cargo/config[.toml]`,
  `src/rust/.cargo/config[.toml]`, `rust-toolchain[.toml]`)
- `canonical_build_root` (`<work_root>/active`)

`Cargo.lock` and crate source blobs are intentionally excluded so Cargo determines
minimal accurate recompilation when crates or dependencies change.

### Timestamp-ordering enforcement and clean fallback

Because `git checkout --detach` stamps all checked-out files with current wall-clock
time, the supervisor aligns unchanged source files to their cached `mtime_ns` only
after strict two-phase timestamp validation:

1. **Publish-time guard (`validate_publish_timestamps`)**: Captures
   `published_at_ns = time.time_ns()` after `kill_build_user(account)` and records
   `max_artifact_mtime_ns`, `max_fingerprint_mtime_ns`, and `source_inventory`
   under the snapshot's `manifest_sha256`. Any non-positive or future-dated
   (`> published_at_ns`) artifact, fingerprint, or source timestamp aborts
   publication (`publish_status: "skipped_unsafe_timestamps"`), removes
   `.staging-<action_id>`, and leaves any prior known-good snapshot intact.
2. **Restore-time Phase 1 (`validate_restore_timestamp_ordering`)**: Before
   mutating any file in `active/source`, captures `restore_now_ns` and
   `min_fresh_checkout_mtime_ns`, restores verified regular files into
   `active/scratch/{target,package-targets}` while verifying `files_sha256` and
   exact destination `fstat().st_mtime_ns` equality with `meta.json` (mismatch ->
   `corrupt_fallback`), and requires non-future timestamps strictly older than
   `min_fresh_checkout_mtime_ns`. If strict ordering cannot be established (future
   artifacts/fingerprints, host clock rollback, or equal-tick tie), the supervisor
   wipes `active/scratch/{target,package-targets}`, leaves `active/source` untouched,
   records `restore_status: "timestamp_ordering_fallback"`, and runs a clean build
   whose valid publication self-heals the cache.
3. **Restore-time Phase 2**: Only after Phase 1 passes, restores cached `mtime_ns`
   on unchanged tracked files while changed/added files retain their verified
   fresh checkout timestamps strictly newer than every cached artifact and
   `.fingerprint` entry.

### Storage layout, bounded retention and clearing

```text
/mnt/data/forge-homelab/ci-cache/          # 0700 root:root
  cache.lock                               # exclusive flock
  scopes/<project>/<scope_dir>/<compat_key>/
    current.json                           # atomic pointer to immutable snapshot
    last_used.json                         # LRU tracking timestamp
    pending-publication.json               # durable rollback/result binding while unfinished
    <published_at_ns>-<action_id>/
      meta.json                            # digest-bound metadata & source_inventory
      target/                              # cached CARGO_TARGET_DIR (excludes ui-assets, ui-qa, tmp; incremental/ at any depth)
      package-targets/                     # cached standalone manifest targets
```

- **Default retention bounds** (configured by `setup-ci.py`):
  `max_total_bytes = 20 GiB`, `max_entries = 8`, `max_age_seconds = 14 days`,
  `seed_from_main = true`, `seed_from_siblings = true`, `timeout_seconds = 2700`.
- **Eviction**: At restore acquisition, remove expired entries first, then
  non-`main` LRU entries before `main`. Protect the selected snapshot, including
  a main seed, and reserve an entry slot for a new writable scope. Explicit
  `prune()` and operator clearing retain the same bounds. Detached restores do
  not perform this entry maintenance.
- **Publication transaction**: Stage a complete immutable snapshot under the
  exclusive cache lock. Before replacing either metadata file, persist a
  supervisor-owned journal with exact old pointer/LRU bytes, new snapshot and
  final result identity. Keep the lock and predecessor through logging,
  build-user quiescence and scratch cleanup. No entry eviction occurs in this
  provisional window. If predecessor plus replacement cannot fit the byte or
  entry bounds, skip optional publication (`skipped_capacity`) and retain the
  previous cache. Reported publication storage includes the temporarily retained
  predecessor; copying into staging also needs temporary disk space.
- **Crash recovery**: Resolve journals before same-action result replay and
  before snapshot GC. Only a durable committed marker plus the exact passed,
  cleanup-confirmed root result authorizes the new pointer. Otherwise persist
  an abort decision, restore previous metadata before receipt access, convert
  an exact provisional success to a failed result, and remove the new snapshot.
  Recovery is repeatable after a crash during rollback or predecessor deletion.
  A malformed unrelated optional cache produces a clean build with
  `corrupt_fallback`/`skipped_error`; uncertain saved-result replay fails closed.
  Unknown storage totals are `null`, never a false empty cache. Root ownership,
  bounded regular-file reads and non-symlink directories remain mandatory.
- **Cancellation**: Recheck both signals and the spool cancellation marker at
  finalization. Pointer replacement is provisional: interruption before durable
  successful completion restores the previous usable cache, including when it
  arrives after pointer or receipt replacement. Commit uses a narrow signal
  guard and checks cancellation after the final marker write. After terminal
  success, deferred predecessor GC may be retried without downgrading a genuine
  passed result. Cleanup/result failures fail or remain unacknowledged; they
  never authorize a provisional publication. `rolled_back` reports zero newly
  published bytes and the restored inventory when it can be measured safely.
- **Systemd**: `setup-ci.py` adds `/mnt/data/forge-homelab/ci-cache` to
  `ReadWritePaths` in `forge-jenkins-ci-agent.service` while keeping
  `ProtectSystem=strict`.
- **Operator cache clear**: `setup-ci.py` installs `clear-ci-cache.py` to
  `/usr/local/sbin/forge-ci-cache-clear` (`0755 root:root`, deliberately excluded
  from `/etc/sudoers.d/forge-ci-check` and `/etc/sudoers.d/forge-native-ci-check`).
  Clear under `executor.lock` + `cache.lock`:

```sh
# Clear all cached scopes
sudo /usr/local/sbin/forge-ci-cache-clear --all

# Clear one project or one specific scope
sudo /usr/local/sbin/forge-ci-cache-clear --project homelab
sudo /usr/local/sbin/forge-ci-cache-clear --project homelab --scope main
sudo /usr/local/sbin/forge-ci-cache-clear --project homelab --scope 'branch/<slug>'
```

The next Jenkins build automatically falls back to a clean rebuild (`restore_status: "miss"`).

### Structured stage timing and cache telemetry

`results_root/<action_id>.json` preserves the strict `checks` list schema and adds:

- `branch_scope`: echoed validated scope (bound in `work_external_delivery.py:verify()`)
- `cache`: `{enabled, provider, project_alias, branch_scope, compat_key, restore_status, restore_fallback_reason, restore_elapsed_ms, restored_bytes, restored_files, publish_status, publish_elapsed_ms, published_bytes, published_files, evicted_entries, cache_total_bytes, cache_entry_count}`
- `stages`: ordered stage timings (`prepare_source`, `cache_restore`, `check_0`, `check_1`, `check_2`, `verify_source_clean`, `cache_publish`, `cleanup`), with `check_0` and `check_1` reporting Cargo-parsed `cargo_compiling_units`, `cargo_finished_count`, and `cargo_compile_ms` (explicitly marked `cargo_reported_cumulative_phase_ms`, distinct from wall-clock stage `elapsed_ms`), plus `ui_qa_stages` from `run.json` on `check_1`.
- **Persistent per-agent cache reuse & stage diagnostics (source M39)**: Under default `branch_policy = "persistent-agent-v1"`, each profile reuses its single persistent branch (legacy `forge/agent/<profile-slug>` or validated override) so `derive_branch_scope()` targets the same `branch/<canonical-branch-slug>` across successive tasks. `verify-candidate.py` emits `correlation_id`, `action_id`, `branch`, and `branch_scope` on lifecycle events, and `work_cicd_artifacts.py` extracts bounded redacted per-stage console error excerpts, structured test counts, and chunked logs for read-only portal inspection (`ide.work.cicd.stage_log`) without widening approval or merge authority. Live verifier/migration deployment remains a separate authorized step.

### Historical measured operator qualification

Isolated cold, warm, post-clear clean rebuild, and deliberate-cancellation
qualification runs on Mission Control (context commit
`d4ea0741d2bf95fee8cb147323b117bcc4d44a00`, qualification proof SHA-256
`546015e03216d369dbddd72f49a8cc8bbb0ffb637d2cb51f4aa405628179435a`) are preserved
in [forge-cargo-cache-qualification-20261121.md](../../../docs/reports/forge-cargo-cache-qualification-20261121.md)
and [forge-cargo-cache-qualification-20261121.json](../../../docs/reports/forge-cargo-cache-qualification-20261121.json):

- **Cold (`restore_status: "miss"`, `publish_status: "published"`)**: total `36m 28s` (`make ci` `20m 25s`, `make ui-qa` `3m 33s`, backend gate `9m 27s`; `390` compiling units; `785` backend tests passed).
- **Warm (`restore_status: "hit_branch"`, `publish_status: "published"`)**: total `21m 25s` (**41.3% shorter**; `make ci` `5m 30s`, `make ui-qa` `2m 31s`, backend gate `9m 22s`; `8` compiling units; `785` backend tests passed; `cache_restore` `79.1s`).
- **After scoped `forge-ci-cache-clear` (`restore_status: "miss"`, `publish_status: "published"`)**: total `34m 59s` (`make ci` `19m 40s`, `make ui-qa` `3m 26s`, backend gate `9m 31s`; `390` compiling units; `785` backend tests passed).
- **Deliberate cancellation (`restore_status: "miss"`, `publish_status: "skipped_cancelled"`)**: failed cleanly with `cleanup_confirmed: true` without publishing a partial snapshot.

These measurements apply to historical canonical helper
`b870302a19504426efac6e46fda38a744138ec852951ae8d96a7d8b1b7b1607d`
(qualification-rendered helper `ef7f917874e626fbcf97ff4c8826acc8c844d996bc45a3e72986f6168c47496a`).
The original receipts and report hashes remain unchanged. They are not timing
measurements for the later publication-transaction changes. A new helper needs
its own correctness evidence, fresh exact-source Jenkins checks and independent
review; retaining identical Cargo copy, key and timestamp code does not relabel
these historical total timings as a new performance measurement.

Official references: [Linux installation](https://www.jenkins.io/doc/book/installing/linux/),
[systemd overrides](https://www.jenkins.io/doc/book/system-administration/systemd-services/),
[headless init hooks](https://www.jenkins.io/doc/book/managing/groovy-hook-scripts/).
