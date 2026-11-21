# Headless Jenkins on Mission Control

Native Jenkins LTS and Java 21 are installed by an explicit local systemd job.
This does not submit a Forge coding workflow, push GitHub source or trigger CI.
Review `install.py` and `bootstrap.groovy` before installing them into the
root-owned `/var/lib/forge-provisioning/jenkins/source` directory and invoking
the installer as root. The current installation is a controller foundation;
build agents and replacement Forge/GitHub verification adapters are separate work.

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
entries, SCM jobs, plugins, build agents and trigger/pipeline configuration remain
separate work. The stopped Grok workflow remains stopped.

GitHub CLI reference: [credential-helper setup](https://cli.github.com/manual/gh_auth_setup-git).

## Mission Control Rust client

The [`forge-jenkins` capability](../../../src/rust/forge-jenkins/README.md) now
provides typed Jenkins I/O through exported factories and registrars. The
[Forge CLI integration](../../../docs/architecture/forge-jenkins.md) adds six
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

Live acceptance verified status, an empty job list, explicit mutation denial and
a typed absent-queue response. No build, pipeline or GitHub workflow was started.
The native service, existing security settings and stopped Grok run remain as
recorded above. Replacing the automatic workflow's verification provider with
Jenkins remains separate from this client implementation.

Official references: [Linux installation](https://www.jenkins.io/doc/book/installing/linux/),
[systemd overrides](https://www.jenkins.io/doc/book/system-administration/systemd-services/),
[headless init hooks](https://www.jenkins.io/doc/book/managing/groovy-hook-scripts/).
