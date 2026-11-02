# Headless Forge IDE QA

Run the existing **18 ordered browser acceptance groups** with one repository-root
command. This is test automation around the workbench, not a new UI, scheduler or
replacement backend. Test-driver JavaScript is not a hand-authored application asset.

```sh
# One-time prerequisites in your development environment (not run by make ui-qa):
cargo fetch --manifest-path src/rust/Cargo.toml --locked
rustup target add wasm32-unknown-unknown
npm ci --ignore-scripts --prefix src/rust/forge-ide-web/tests/browser
# An installed, sandbox-capable Google Chrome, Node20+, Python3, Git and stable Rust.
make ui-qa
```

`make ui-qa` rebuilds matching generated assets and real WASM, builds the agent and
owned fixture, then launches headless Chrome with its sandbox **enabled**. It selects
the fixed full browser suite; Cargo package/feature overrides cannot narrow it.
No credentials, configuration, user project or running service is required. Only
new disposable localhost fixtures and ephemeral test TLS identities are used.

Every invocation prints `QA_OUTPUT <directory>` and allocates a **new** run under
ignored `src/rust/target/ui-qa/`. Set `QA_OUTPUT_ROOT=/absolute/owned-output-parent`
if needed; prior reports/screenshots are never overwritten or reused. Output-directory
reservation failure exits without changing existing evidence. No dependencies,
browser or OS policy are installed/changed automatically. Keep normal Chrome
sandbox support: never add `--no-sandbox`, ignore certificate errors, disable
AppArmor or alter host policy to turn a blocked run green.

The [existing hosted workflow](../../../../../.github/workflows/ide-ui-browser.yml)
uses the same `make ui-qa` entry on Ubuntu24.04 and the runner-installed Chrome
with its supported sandbox profile. It runs on relevant pull requests, main and
`validation/forge-ide-ui`, and can be dispatched manually. Actions are pinned,
permissions read-only, and no operational secrets/deployment are involved. Main
acceptance still requires both same-head Rust and actual browser gates, plus
review of the generated screenshots; a successful helper test is not that gate.

## Read the result

| Artifact in the printed run directory | Meaning |
| --- | --- |
| `run.json`, `run.md` | Build/preflight/browser stage status and duration, revision, dirty flag and source-byte fingerprint; exists even when prerequisites fail |
| `browser/result.json` | Versioned structured 18-group records: stable ID, scope/backend, dependencies, timestamps/duration, passed/failed/not_run, primary failure and independent cleanup outcomes |
| `browser/summary.md` | Readable scenario and cleanup table, safe failure category/stage |
| `browser/junit.xml` | Acceptance cases plus separate harness-error suite for setup/cleanup failures; not-run cases are skipped, not passed |
| `browser/*.png` | Known desktop/narrow/partial/empty/denied and controlled-state screenshots; optional masked `failure.png` only when safe capture is possible |

`schema_version: 1` is the reporting contract, not the application/WASM or HTTP
protocol version. [catalog.mjs](catalog.mjs) is the authoritative ordered scenario
inventory. IDs are stable labels for regressions; groups share state within the
journey and are **not independently selectable tests**. A first failure stops the
sequence: later groups remain `not_run`. Cleanup errors independently fail the run
without hiding the original failure. Interrupt/deadline handling attempts each owned
resource's bounded teardown; a blocked cleanup is reported, never claimed successful.

The driver fingerprints all five generated assets, their manifest and the two
actual executable files. The root helper fingerprints tracked and nonignored source
paths/bytes in addition to Git HEAD, so dirty work is not presented as a clean commit.
The helper compares the source fingerprint again at completion: changed or unknown
source identity makes the overall run fail, even if browser assertions passed. Keep
custom output outside tracked/unignored source paths to avoid changing that identity.
Hashes identify inputs, not a signature or proof that a published commit ran.
Raw exception messages/stacks, page console output, URLs, headers, network bodies,
traces, storage state, fixture stdout/stderr and keys are **not** report artifacts.
Failure reports use fixed categories and the current stable scenario ID. This is
intentionally less detailed than a raw trace; inspect the named assertion locally
when deeper diagnosis is needed. Never add credentials to debug output.

The hosted artifact `forge-ide-browser-qa` includes only explicit report filenames
and known fixture PNGs, for 7days; the workflow also adds the safe Markdown reports
to its job summary. It does not upload the output tree, build/package-manager logs
or fixture directories. Screenshots use only test-owned source data, mask pairing
and editable controls when capturing failures, and are skipped if a safe state
cannot be established. Review images before sharing them beyond the test artifact.

### Ordered coverage

| IDs | Evidence |
| --- | --- |
| `real.report.partial`, `real.navigation.keyboard`, `real.filters.safe_text`, `real.pagination.problems` | Actual report, navigation, full bounded-report filters, safe labels and problem cross-navigation |
| `real.panes.desktop`, `real.panes.narrow` | Pointer/keyboard resize/collapse/reset and narrow pane interaction |
| `probe.report.stale_actions`, `probe.page.lifecycle` | Real WASM stale-event acceptance; explicitly manipulated lifetime probes plus real reload/history |
| `real.report.empty`, `real.admission.denied` | Real empty project and independently denied project |
| `injected.requests.cancel_retry`, `probe.presentation.ordered_drafts`, `injected.requests.revisions`, `injected.requests.refresh_focus` | Injected timing, local cancellation, repeated events, input acknowledgements and focus |
| `injected.failures.unavailable_timeout`, `probe.worker.fatal`, `injected.catalog.empty`, `probe.session.missing` | Separately controlled error, fatal, catalog and session-lifetime cases |

## Evidence scopes

- **Real-agent journey:** disposable source directories and ephemeral test TLS
  identities; browser → generated worker → real WASM → native admitted host →
  independently authorized mTLS agent. Covers partial structural reports, complete
  empty projects, denied projects, HTML-like/long/native names, blank/binary/parse-
  unavailable files, full-report file and symbol filtering, hidden selection,
  bounded pages, Problems navigation, arrow/Home/End/typeahead focus versus Enter
  selection, keyboard pane resizing/reset/collapse/reopen, 1440px desktop and
  760px narrow layouts, and refresh/browser leave-return. Navigation/filter/pane
  actions must not create inspection requests.
- **Actual WASM stale-intent probe:** observes only local UI events in ephemeral
  browser memory, then replays old row/filter events after report/file changes.
  This uses the real model and transport, not a replacement worker. Report token
  context is never written into test output, screenshots or logs.
- **Controlled native port:** `loopback_fixture --faults` retains native host
  session admission and real WASM but injects delayed/unavailable responses. It
  proves local cancellation, rapid selections/repeated clicks, timeout, pending
  refresh and focus behavior, **not remote transport correctness or remote task
  termination**.
- **Controlled input latency:** temporarily queues genuine worker/WASM view
  messages, then releases them in their original FIFO order. The `f → fo → f`
  backspace and clear cases check that an earlier equal-value echo cannot
  acknowledge a newer draft. Neither WASM nor its semantic projection is replaced.
- **Controlled adapter fatal:** calls the generated worker's fail-closed routine
  after accepted data and checks worker termination plus protected DOM clearing.
  This is fatal-presentation/lifetime coverage, not spontaneous ABI corruption.
- **Controlled catalog-only case:** replaces one successful Resume catalog with
  zero allowed projects. Production `HostConfig` intentionally disallows empty
  configured project lists and is unchanged. This case proves the UI's bounded
  empty-choice state only; it does not establish native admission behavior.
- **Page lifetime:** real reload and leave/return are combined with a deterministic
  `pagehide/pageshow` persisted-event probe to assert synchronous DOM clearing.
  The synthetic persisted flag does not claim the browser actually used bfcache.

The driver bounds the browser journey to 180seconds, then independently attempts
bounded cleanup. The root wrapper allows 300seconds for the driver, followed by
a 60-second graceful-exit window; a forced driver kill is an explicit
`driver_killed_cleanup_unconfirmed` failure, not proof its fixtures were released.
These are browser deadlines, not compile-time limits; Cargo builds retain normal
local cancellation and the hosted job has a 20-minute ceiling. Fixtures/process groups are
owned by this invocation and receive shutdown independently of browser cleanup.
No remote task-cancellation or production deployment claim follows from this test.

## Direct driver / extending a scenario

For an already built matching bundle, the lower-level invocation remains:

```sh
FORGE_UI_BROWSER_CHANNEL=chrome node src/rust/forge-ide-web/tests/browser/journey.mjs \
  /absolute/generated-assets /absolute/forge-agent \
  /absolute/loopback_fixture /absolute/new-browser-output
```

The final directory must **not exist**; its parent must exist. Unlike the root
helper, direct use does not rebuild inputs. Failure screenshots are opt-in here
with `FORGE_UI_QA_FAILURE_SCREENSHOT=1`; the root command enables safe capture.

When changing coverage: update [catalog.mjs](catalog.mjs), mark start/pass boundaries
in [journey.mjs](journey.mjs), retain existing assertions and label any injected
transport/response/lifecycle evidence. Keep requests on the actual admitted gateway;
do not substitute a fake WASM or silently make a real-agent case a mocked case.
A new artifact filename requires explicit workflow allowlisting and a privacy review.

## Focused checks (not browser evidence)

```sh
make ui-qa-check  # reporting/orchestration unit tests, no browser required
python3 -B -m unittest discover -s src/rust/scripts/tests -v
node --check src/rust/forge-ide-web/tests/browser/journey.mjs
cargo test --manifest-path src/rust/Cargo.toml \
  -p forge-ide-web-host --example loopback_fixture --locked
```

The native fixture tests validate that the exact real-agent source corpus projects
to a bounded portable report with >256 rows, a beyond-page match, valid empty text,
binary/parse-unavailable distinctions, encoded names and a complete empty root.
They also check the separately injected port's response context/error categories.
Hosted browser execution and visual review of all generated screenshots remain a
separate required acceptance gate.
