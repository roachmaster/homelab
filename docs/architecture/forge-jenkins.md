# Jenkins client and Forge CLI

Implemented source: the optional [`forge-jenkins`](../../src/rust/forge-jenkins/README.md)
Rust capability implements `forge_core::io_client::IoClient`. Its public `Config`,
`Credentials`, `ClientFactory`, `register` and opt-in `register_http` compose with
Forge's existing bootstrap and provider contracts. No dependency selection or
global registry lookup occurs in a request handler. The library default omits
native HTTP; the terminal's explicit `jenkins` feature selects the Ureq transport.
Existing terminal features and default dependency profiles remain unchanged.

## Construction and ownership

Application bootstrap selects a typed configured factory, stages the registrar,
validates/freezes the plan, then creates the selected client. Hosts choose
`Singleton` or `Transient` ownership. The independent Cargo consumer imports the
public library exports and exercises both policies with a substituted transport.
This verifies construction and resource identity rather than relying on a diagram
or private example include.

The terminal selects a fixed configuration-file path at startup. Its factory is
registered and frozen without reading that file. The first admitted, validated
Jenkins request loads the bounded configuration and private token through the
terminal's selected client. It explicitly creates the configured native client
and retains it for that CLI/application lifetime. No runtime dependency registry
is consulted. Help, command discovery and unrelated node/disk/jobs actions do not
load credentials or call Jenkins. Restart a CLI session to load changed files.

## Command flow

Terminal metadata/argument decoding constructs the application-owned
`JenkinsCommand`. `Application::invoke` constructs a header/envelope and uses the
same admitted bridge as direct typed callers. Header destination/version and
payload kind must match before the binding invokes its Task. The Task prepares
and validates the typed request, then calls the injected Jenkins `IoClient`.
The concrete adapter encodes it for Jenkins and projects a bounded typed response.
The terminal formats that outcome using its existing console I/O boundary.

| Forge route | Jenkins effect | Completion meaning |
| --- | --- | --- |
| `jenkins.status` | GET controller information | One observed response |
| `jenkins.jobs` | GET visible job summaries | One observed list |
| `jenkins.build` | POST a prepared job and parameters | HTTP201 plus a validated queue ID; build is queued |
| `jenkins.queue` | GET one queue item | One observation; absent flags remain unknown |
| `jenkins.build-status` | GET one numbered build | One observation; no background polling |
| `jenkins.cancel` | POST queue cancellation | Request accepted; no claim that a running build stopped |
| `jenkins.queue-list` | GET a bounded queue snapshot | Correlation observation, including a truncation flag |
| `jenkins.recent-builds` | GET up to 50 numbered builds for one configured job | Correlation observation, including a truncation flag |
| `jenkins.stop-build` | POST a numbered build's stop action | Request accepted; cleanup must still be observed |

Mutations are denied by default. An explicit client configuration opt-in is
required for build or cancellation. Unexpected/lost write receipts, transport
errors and timeouts require reconciliation, with no automatic retry. This client
is used by Forge's durable delivery providers; it does not itself own workflow
state or infer CI completion. The stopped Grok workflow remains stopped.

## Native Gemini workflow

Bootstrap selects `hosted_provider: "jenkins"` and
`native_delivery_mode: "branch-first-v1"`. `jenkins_delivery_config` selects the
pinned Rust CLI, native verifier/job and private credentials. Ready ports are
injected into the publisher; handlers never look up a transport registry.
Existing configurations retain `github-actions` and `integrate-first-v1`
defaults. The external Muse job/verifier remains separately pinned.

The native Plan/Implement recipe, execution policy, independent Codex reviewer
and frozen budgets stay unchanged. Logical gate names `check`, `pr-fast-gate`
and `real-browser` remain compatibility IDs; observations identify Jenkins.

```mermaid
sequenceDiagram
    actor U as Owner
    participant P as PostgreSQL workflow/events
    participant G as Gemini
    participant C as Codex reviewer
    participant V as Trusted branch/CI/merge worker
    participant GH as GitHub repository
    participant J as Jenkins through Rust IoClient
    U->>P: Submit assignment
    P->>G: Plan / bounded revisions
    G->>P: Immutable Plan result
    P->>C: Independent Plan review
    C->>P: Approve exact Plan
    P->>G: Implement
    G->>P: Stop and reconcile source
    P->>V: Lease verification
    V->>GH: Read current main
    V->>V: Three-way integrate in private clone; pin authored/main/candidate
    Note over V,P: A conflict yields unapproved source feedback, with no push or build
    V->>GH: Push exact integrated source to its owned branch
    GH-->>V: Confirm remote head
    V->>P: Commit idempotent branch_pushed event
    P-->>V: Event receipt under current lease
    V->>J: Submit correlated exact-source build
    J->>J: Main-ancestor preflight, then all fixed gates
    J-->>V: Actual build result and cleanup proof
    alt Build fails
        V->>P: Failure event and bounded author repair
        P->>G: Actual build reference / saved source
        G->>G: CLI status and paginated logs; correct source
        G->>P: Stopped correction restarts push/event/build
    else Build passes
        V->>P: Record exact-source verification
        P->>C: Independent full-candidate code review
        C->>P: Approval or bounded repair
        P->>V: Authorized publication / merge
        V->>GH: PR for already-pushed checked branch
        V->>J: Reobserve same build receipt
        V->>GH: Guarded exact-tree main update
        alt Main update confirmed
            V->>P: Merge receipt and completion
        else Confirmed merge failure
            V->>P: New unapproved repair source and failure event
            P->>G: Resume with actual build reference
        end
    end
```

Source integration never updates the native worker workspace or substitutes its
base/digest/stop proof. A version2 private receipt distinguishes the exact authored
tree from the integrated candidate and freezes observed main. Replay verifies both
trees and parent ancestry, reuses that same candidate, and leaves historical v1 or
already pushed/build-bound receipts intact. A conflict commits only the real
unresolved merge as unapproved repair evidence, preserving nonconflicting main
files. Its source findings report no checks and no native Jenkins identity; normal
repair policy and owner-continuation fences still apply. Later main races retain
the existing independently verified build/merge feedback path.

Jenkins runs all fixed commands once per admitted verification action. Later
publication/merge stages reobserve the same action. Changed source or an
explicitly admitted bounded retry requires a fresh action. Native and Jenkins
manifest digests retain their distinct contracts and immutable binding.

Migration24 supplies narrow CI context/status functions. Migration25 adds the
committed branch-pushed event, authenticated build-context reader, bounded merge
failure repair and once-only replay of a genuinely stopped pre-CI conflict.
Existing recipes, Plan counters and historical receipts are preserved. The
committed event receipt and live review lease are required before Jenkins.

The native job `forge-native-candidate` first checks that pinned current main is
an ancestor of the pushed source. An outdated branch produces a real failed
build receipt before expensive compilation. Gemini gets an explicitly
unapproved source containing the real Git merge result: nonconflicting main
files and any unresolved conflict markers. It corrects that source through its
normal CLI/file-edit scope, then the next stopped result is pushed and tested.

Confirmed merge failure returns through the same bounded repair loop. Unknown
remote write acknowledgements are reconciled before admitting a new writer.
Code review compares the complete candidate with actual main, including feature
code inherited through unapproved repair seeds. Only a genuine successful native
merge advances `refs/forge/verified-main`.

Restart reuses the prepared source SHA, committed branch event and journaled CI
action. A durable intent precedes each remote write; an uncertain push/build is
never blindly repeated. Cancellation waits for actual queue/process cleanup.

## Agent build inspection CLI

The worker's explicit `build_results_root` selects a worker-owned artifact reader.
Its scoped database port binds the current repair feedback to its source review.
The reader verifies the private action, actual root receipt and log hashes.
Only sanitized immutable evidence enters the attempt; the raw results root is
masked from model processes. No Jenkins/database credentials enter the scope.

```sh
python3 -I -B /run/forge-coding-attempt/build_info.py status
python3 -I -B /run/forge-coding-attempt/build_info.py logs --offset 0 --limit 16384
```

`status` reports the actual job/build/action, source/main identities and outcome.
`logs` reports `text`, `next_offset` and `eof`; the agent reads the pages it needs,
up to 64KiB each from a bounded 64MiB log. The prompt carries the build reference
and CLI command instead of embedding a long diagnostic transcript.

## Authentication and transport

Jenkins uses preemptive Basic authentication with username and API token.
API-token requests are CSRF-exempt; the adapter leaves Jenkins's CSRF protection
enabled. [Official scripted-client authentication](https://www.jenkins.io/doc/book/system-administration/authenticating-scripted-clients/),
[CSRF behavior](https://www.jenkins.io/doc/book/security/csrf-protection/).

Mission Control has a named token, **Mission Control - Forge CLI**, expiring
2027-05-20. The provisioning script used a password-authenticated, cookie-bound
crumb once to issue it. Token values remain outside Git in private files. The
local configuration is `/home/lrocha/.config/forge/jenkins/mission-control.json`;
the token file is adjacent. The token inherits the existing `forge-admin`
permissions. Read-only CLI policy is not a server-side permission restriction.
No bearer-authentication gateway, new server user or authorization plugin was added.

The native transport permits numeric loopback HTTP for local Mission Control;
other destinations require verified HTTPS. It disables ambient proxies,
redirect following and retries. Queue Locations must match the selected origin
and context path and are reduced to a canonical positive ID. Job segments and
parameters are bounded and encoded, never interpreted as arbitrary URLs or shell.
Response bodies/headers and network deadlines are bounded; authentication and
parameter bodies are redacted from Debug/errors. Private file loading rejects
symlinks, unsafe ownership/modes, hardlinked secrets and nonregular inputs.

## Running and verification

Build with the installed supported Rust toolchain, for example:

```sh
RUSTUP_TOOLCHAIN=1.97.1 make run FEATURES=jenkins ARGS='--no-disk --jenkins-config /home/lrocha/.config/forge/jenkins/mission-control.json jenkins.status'
```

See the [terminal guide](../../src/rust/forge-terminal/README.md) for argument
syntax and the [deployment guide](../../deploy/mission-control/jenkins/README.md)
for native service and credential setup. Source/transport fixtures, the separate
public consumer, CLI admission/private-file tests and live read-only acceptance
are distinct evidence. Fake HTTP tests never submit a real Jenkins build.

Mission Control's installed `forge` command is qualified against Jenkins 2.580.2.
The `forge-exact-candidate` job runs on the dedicated single-executor
`forge-mission-control-ci` node; the controller retains zero executors. External
Muse delivery already uses that job. A real full-suite smoke build passed on
2026-11-21; it proves only its recorded source, not a later task's candidate.
Native-provider tests use owned fixtures and do not impersonate production
model approvals or Jenkins results. Deployment and live-task evidence are
recorded in `AGENT_WORK.md`.
