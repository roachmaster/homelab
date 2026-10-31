# Bounded local jobs

The optional `jobs` feature contains a **volatile, single-owner, explicitly stepped**
pure-counter capability inside `forge-local`. No new core crate, OS process, shell,
network, background scheduler, async runtime or persistent store is introduced.
`Task`, `IoClient`, command descriptions, bridge and provider contracts are unchanged.

## Use

```bash
# Repository root
make run ARGS='--no-disk jobs.submit --help'
make session ARGS='--no-disk'
# Within the session:
jobs.submit example 130
jobs.list
# Copy the returned incarnation:sequence ID:
jobs.inspect ID
jobs.step ID
jobs.cancel ID
jobs.step ID
jobs.inspect ID
```

A single-shot submit exits and loses its records. Keep a session or one embedded
`Application` alive to inspect/advance the same job. Only `jobs.step` executes a
bounded quantum; blocked input, help, list and inspect do not run computation.
Maintenance may expire queued/retained records during later commands.

Every operation passes through the descriptor-derived application catalog and the
same header-selected bridge/variant gate. Terminal help uses admitted discovery,
not an unchecked engine path. Descriptions are authored metadata, not reflection.

Default terminal builds include disk and jobs. `--no-jobs` skips jobs registration;
`--no-disk` skips disk. `--no-default-features` compiles neither optional module;
add `--features jobs` for counter jobs without disk. Existing
`Application::bootstrap` stays node/optional-disk only; `bootstrap_with_jobs`
explicitly selects jobs. This proves module/feature omission, not that the entire
application crate disappears from its consumer's dependency tree.

## Typed assembly and bounds

`JobsConfig::local(incarnation)` supplies finite defaults:

| Limit | Default |
| --- | --- |
| Queued / running / outstanding per owner | 16 / 4 / 16 |
| Total records, including retained terminal intent/results | 64 |
| Counter target / work units per step | 1,000,000 / 64 |
| Queue TTL / running budget / terminal retention | 300s / 30s / 600s |

Validation rejects incompatible or zero limits, record capacity above 65,536,
quantum above 4,096, time limits above one day, and invalid identifiers. Incarnation
bases are at most 42 ASCII identifier bytes; owner/key identifiers are at most 64.
Input, progress and result are fixed-size counter values. These quotas bound the
engine's retained records, not arbitrary allocations/CPU in trusted replacement
code or response copies retained by callers.

`jobs::register` stages a cohesive `JobsFactories` group without program, policy,
clock or engine callbacks. Application bootstrap stages all selected groups,
validates/freezes, then creates clients/service explicitly. A singleton owns the
ready jobs handle; bindings acquire it only after the shared ingress checks.
Invalid jobs configuration precedes selected client creation; a partial host is
never published. Factory/provider roles are separate from runtime route lookup.

`register_with` / `JobsFactory::with_components` accept a narrow program, local
policy and clock for typed substitution. Factories cloned from the same factory
share a checked creation counter, appended to its configured incarnation base.
Separately configured factories/processes still require fresh bases. The terminal
uses a checked process/time/sequence label: best-effort across restarts, not durable
global identity or authentication. Mismatched incarnations reject stale IDs; reusing
a base externally can defeat that protection. No restart recovery is claimed.

## Lifecycle and intent

- Submit reserves queue/record/intent capacity together, prepares owned program
  state without running it, then commits volatile acceptance. Failed preparation
  removes the reservation and returns no receipt; IDs never wrap or get reused.
- A queued job starts only through an explicit step after capacity, time and
  execution-policy checks. Queue expiry never executes work. Policy denial or
  unavailability produces a bounded Failed result without an automatic retry.
- A running cancel returns **Requested**, not Cancelled. The next cooperative step
  sees the request and the counter stops; a completing conforming step may instead
  win. Queued cancel stops before any step. Terminal jobs never execute again.
- Running-budget expiry requests stop at a checkpoint; it cannot force-stop code.
  Time is resampled after policy/preparation/step callbacks; acceptance and terminal
  retention use their commit times, not stale callback-entry timestamps. Backwards
  clock observations fail closed rather than refreshing a budget.
- No engine borrow crosses policy/program/clock/custom-destruction callbacks.
  Reentrant calls return Busy and in-flight work still consumes active capacity.
  Panics propagate and poison the jobs handle if caught; this is not rollback,
  safe recovery, process supervision or preemption of arbitrary Task code.
- Host exit drops pure owned state and loses every record. There are no background
  workers to join and no managed distributed drain protocol.

The same owner/key/total in one immutable host configuration returns its original
receipt; a changed total conflicts without replacement. Request IDs are not intent
keys. Active keys never expire. Terminal result and intent are retained **together**
for the configured window; unexpired records are never evicted to admit new work.
After retention expires and a later call performs maintenance, lookup is NotVisible
(unknown or hidden) and key reuse may accept new work. There is no universal
exactly-once or durable acceptance guarantee.

## Local ownership is not authentication

Ordinary terminal calls use `local-process`. Trusted embedding code may select
`LocalPrincipal` through `invoke_as_local` / `dispatch_as_local`; the payload has
no owner field. The engine checks actual stored ownership independently of an
injected policy for inspect/list/step/cancel. The default policy allows local
operations, but one owner cannot operate on another owner's records through that
interface.

These APIs deliberately trust their in-process caller, which can choose another
label. They are **not** remote entrypoints, credential verification, OS-user
isolation or production authorization. All selected descriptions remain visible
in local mode. Secured discovery and remote authorization require separate work.

## Compatibility and deferred work

The feature adds variants to public exhaustive application enums (`Command`,
`CommandKind`, `Output`, `RunError`, `BootstrapError`). Enabling it can require
updating downstream exhaustive matches; this is not silently source-compatible.
Existing constructors and the seven execution/description libraries stay unchanged.
These unpublished 0.1 application APIs are not a stability pledge.

The broader [foundation design](../../../docs/architecture/foundation-interfaces.md)
still proposes generic jobs, production identity, remote addressing, persistent
intent recovery and event-loop hosting. This fixed pure-counter slice establishes
bounded local lifecycle semantics first; it does not implement those later APIs.
