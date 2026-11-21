# Headless Jenkins on Mission Control

Native Jenkins LTS and Java 21 are installed by an explicit local systemd job.
This does not submit a Forge coding workflow, push GitHub source or trigger CI.
Review `install.py` and `bootstrap.groovy` before installing them into the
root-owned `/var/lib/forge-provisioning/jenkins/source` directory and invoking
the installer as root. The installation now also has a dedicated CI agent and
the trusted `forge-exact-candidate` pipeline described below.

## Current CI pipeline

The exclusive `forge-mission-control-ci` node has one executor and label
`forge-ci`; controller executors remain zero. `forge-jenkins-ci-agent.service`
connects through WebSocket. `forge-exact-candidate` invokes the root-owned
`/usr/local/libexec/forge-ci-check` wrapper with seven validated correlation and
source parameters. The installed verifier reads a digest-bound bundle, verifies
the exact commit/tree and fixed gate manifest, then runs `make ci`, `make ui-qa`
and the complete Python backend suite. Candidate commands run as the dedicated
unprivileged `forge_ci` account with private network, read-only source and owned
outputs. Jenkins and model credentials are outside that boundary.

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
    <published_at_ns>-<action_id>/
      meta.json                            # digest-bound metadata & source_inventory
      target/                              # cached CARGO_TARGET_DIR (excludes ui-assets, ui-qa, tmp)
      package-targets/                     # cached standalone manifest targets
```

- **Default retention bounds** (configured by `setup-ci.py`):
  `max_total_bytes = 20 GiB`, `max_entries = 8`, `max_age_seconds = 14 days`,
  `seed_from_main = true`, `timeout_seconds = 2700`.
- **Eviction**: Expired entries (`> max_age_seconds`) are removed first, then LRU
  entries until `entry_count <= max_entries` and `total_bytes <= max_total_bytes`,
  evicting non-`main` scopes before `main`.
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

Official references: [Linux installation](https://www.jenkins.io/doc/book/installing/linux/),
[systemd overrides](https://www.jenkins.io/doc/book/system-administration/systemd-services/),
[headless init hooks](https://www.jenkins.io/doc/book/managing/groovy-hook-scripts/).
