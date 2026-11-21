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
the original installer default was 1,800 seconds. Compile targets are still per
action until the separately assigned Cargo-cache feature is verified and merged.
Do not describe the existing registry/toolchain/browser caches as a compiled
artifact cache.

External Muse delivery uses the typed Rust Jenkins client through the trusted
delivery worker. Native Gemini uses the publisher's explicit `jenkins` provider
selection and private `jenkins_delivery_config`. See the
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

Official references: [Linux installation](https://www.jenkins.io/doc/book/installing/linux/),
[systemd overrides](https://www.jenkins.io/doc/book/system-administration/systemd-services/),
[headless init hooks](https://www.jenkins.io/doc/book/managing/groovy-hook-scripts/).
