# Headless Forge IDE QA

Run the existing **28 ordered browser acceptance groups** with one repository-root
command. This is test automation around the workbench, not a new UI, scheduler or
replacement backend. Test-driver JavaScript is not a hand-authored application asset.
The Agents fixture also covers host-composed MCP connections (Online, Stale and
Never seen) separately from native worker readiness, using only disposable reader
credentials and controlled heartbeat observations. The Agents journey covers separate 50-event history pages with durable correlation,
keyboard disclosure preservation, failed-page retry and external database workflow
phases through reviewed merge, using deterministic owned runner fixtures.

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
The expanded full journey has a 270-second deadline within the Python
supervisor's 300-second limit. Individual assertions keep their existing bounds;
deadline failure still stops the sequence and cleans up owned resources.

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
| `browser/result.json` | Versioned structured 28-group records: stable ID, scope/backend, dependencies, timestamps/duration, passed/failed/not_run, primary failure and independent cleanup outcomes |
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
| `real.report.partial`, `real.navigation.keyboard`, `real.filters.safe_text`, `real.pagination.problems` | Single-project automatic opening; real source reads; phone file/symbol search and direct source navigation; safe source text; actual report, source-shape inventory, keyboard focus, bounded filters and problem cross-navigation |
| `real.panes.desktop`, `real.panes.narrow` | Pointer/keyboard resize/collapse/reset and narrow pane interaction |
| `probe.report.stale_actions`, `probe.page.lifecycle` | Real WASM stale-event acceptance; explicitly manipulated lifetime probes plus real reload/history |
| `real.report.empty`, `real.admission.denied` | Real empty project and independently denied project |
| `injected.requests.cancel_retry`, `probe.presentation.ordered_drafts`, `injected.requests.revisions`, `injected.requests.refresh_focus` | Injected timing, local cancellation, repeated events, input acknowledgements and focus |
| `injected.failures.unavailable_timeout`, `probe.worker.fatal`, `injected.catalog.empty`, `probe.session.missing` | Separately controlled error, fatal, catalog and session-lifetime cases |

The connected application and native Design headers share four views in order:
**Agents, Workbench, Home, Design**. Agents is the default entry; legacy `#chat`
and `#mail` entries resolve there without acquiring Chat or Mail requests. Home
keeps its focused launcher header and links to Agents without a duplicate IDE strip.
The Agents journey remains inside `real.report.partial`, covering keyboard entry,
active-tab semantics, same-tab Home handoff, retained task drafts and browser
Back/Forward. A valid legacy Mail cache remains inactive and cannot overwrite a
current Agents draft or replay a request.

The controlled Agents relay also supplies a Grok profile through the native
catalog and usage ports. Its unobserved readiness, allowance and counters remain
Unknown/Not reported, without a quota meter. The actual WASM journey verifies
this generic card at 320/390/1440px; it does not invoke a real Grok CLI.

### Agents triage fixture coverage

The same fixed `real.report.partial` group also runs an isolated, read-only
Agents triage journey through real WASM and the admitted native host. Its
owned relay seeds native and Muse assignments directly in disposable memory.
It performs no assignment submission, retry, cancellation, acceptance or merge.
The corpus checks:

- A visible keyboard-accessible Needs attention scope alongside Active,
  History and All assignments; counts come from the controlled service response.
- Readable whole-word titles, phase, agent and last update, with the exact full
  assignment retained in detail.
- An automatic plan retry at 1 of 20 with 7 cumulative revisions; an open retry
  at 20 of 20 that remains Active; and an authoritative escalation at only
  1 of 20 that still needs attention. A cancelled author under an escalated
  workflow remains distinct from user cancellation.
- Native automatic revision, Muse independent review and an operator action in
  Now / Why / Next. Worker ready remains separate from Main verification queued.
- Accepted rows retained after a failed list refresh, empty Active, a delayed
  detail read and a failed detail read without inherited progress or success.
- The same focused DOM row, selection and scroll position across background
  polling and keyboard return from detail. Native disclosures expose full
  assignment, stages, workflow evidence, technical IDs and event history.
- Inbox and detail at **1363 × 844**, **320 × 844** and **390 × 844**. Full
  stage evidence is a deliberate vertical list; the current stage remains
  available in the compact summary before opening the evidence disclosure.

Known fixture captures are explicitly allowlisted by the hosted workflow:
`ide-agents-triage-inbox-{1363,320,390}.png`,
`ide-agents-triage-detail-{1363,320,390}.png`,
`ide-agents-triage-attention-390.png` and `ide-agents-triage-muse-390.png`.
Existing `ide-agents-overview-{phone,desktop}.png` and
`ide-agents-stage-diagram-{phone,desktop}.png` preserve the earlier controlled
scenarios for comparison with a baseline run. These are capture instructions,
not evidence that a run or screenshot review has occurred.

For before/after evidence, run the unmodified fixed gate in a separate clean
baseline checkout and again on the proposed source, retaining both `QA_OUTPUT`
directories and their revision/fingerprint reports. Compare the common inbox
and native-stage fixture states, then inspect the additional triage captures.
Do not copy baseline PNGs into a current run or present missing captures as
passed. The ordinary PR workflow runs harness checks but **skips full browser
execution**; a successful PR job is not screenshot or interaction evidence.
Use the existing workflow dispatch on the feature ref, or run `make ui-qa` in
an environment with its documented prerequisites, for the full gate. No gate
condition or deployment path is changed by these tests.

The current portal does not expose Chat, model selection, saved-chat management,
contextual inference or model-generated edit proposals. Assertions require those
entry points and screens to be absent. The request observer counts inference,
history, model, run and proposal dispatches and requires zero throughout the
retirement path. Existing native/backend tests retain the archived capability's
contracts; the browser suite no longer claims those retired screens are live.

`probe.page.lifecycle` checks an unconfigured Agents host at 320/375/390px, legacy
entry redirects, synchronous DOM clearing, source re-verification and host restart.
Its owned relay seeds a saved Chat snapshot containing multiple drafts, committed
messages, selected model and an admitted pending-run identity. Authenticated
`preserve_chat` restores and later saves must round-trip that snapshot unchanged,
without linked-history hydration or resubmission. Raw stale Chat events are sent
through the actual worker/WASM and cannot mutate it or acquire Chat I/O. A new
browser tab gets neither the old bookmark nor dormant snapshot. These are
controlled persistence proofs, not a claim about production database contents.

The fixed 28-group inventory and ten viewport cases remain unchanged. Historical
Chat/Mail design articles and dated wireframe images are reference coverage.
Direct file editing remains live: its browser path verifies a dirty editor survives
an Agents/Workbench switch, a lost save reply recovers through the same-operation
status check, reload preserves the committed file, and Undo restores its prior text.
Model-generated proposal/apply coverage is retained by the native tests rather
than represented as a current portal action.

The same Agents journey covers finished Plan decisions: Resume approves the exact
saved artifact for Implement mode, Revise requires review-note feedback, and Cancel
ends the plan without execution. Legacy completed plans retain these controls.
The fixture deliberately drops an acknowledged decision response, reloads the tab,
and verifies retry keeps its artifact/revision/idempotency identity. Implementation
publication is a separate bounded detail: Pending/Checking/Publishing is polled even
at an unchanged task revision, generic Accept/Resume stays unavailable until it
settles, and a Published result links only to a validated GitHub pull request. These
are controlled backend/browser proofs; no real coding agent or remote PR is invoked.

## Evidence scopes

- **Real-agent journey:** disposable source directories and ephemeral test TLS
  identities; browser → generated worker → real WASM → native admitted host →
  independently authorized mTLS agent. Covers partial structural reports, complete
  empty projects, denied projects, HTML-like/long/native names, blank/binary/parse-
  unavailable files, full-report file and symbol filtering, hidden selection,
  bounded pages, Problems navigation, arrow/Home/End/typeahead focus versus Enter
  selection, keyboard pane resizing/reset/collapse/reopen, 1440px desktop and
  760px narrow layouts, and refresh/browser leave-return. Navigation/filter/pane
  actions must not create inspection requests. Opening a readable file may issue
  one separately admitted source read; search and source-page changes do not.
  A separate single-project real-agent fixture proves automatic opening after
  pairing and resume, case-insensitive symbol search, direct phone source display,
  highlighted declaration lines, Back to files, and literal HTML-like source text.
  A 600-line UTF-8 file exercises bounded forward/backward source pages without
  additional reads, with visible keyboard focus on the paging control.
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
  A controlled loopback workspace relay stays up while separate native hosts serve
  the same browser origin. The original tab preserves its dormant Chat data and
  reauthorizes the selected source after new pairing. A new tab has no old
  Workbench selection or dormant data. The relay uses only test-owned memory;
  no inference turn is invoked.

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


### iPhone and tablet viewport matrix

The original 760×1000 narrow regression remains intact. Ten additional sequential
`real.viewport.*` groups reuse the same accepted real-agent report. Every profile
gets a separate JSON/Markdown/JUnit result; each successful case captures its own
allowlisted masked fixture PNG. A failed case may only have the safe `failure.png`
capture, and not-run cases have no image. The report includes the profile name,
exact CSS-pixel width/height and expected screenshot filename, so failure and
not-run states identify the affected orientation. Matrix failures also retain a
fixed checkpoint label. A focus-check failure may include the latest bounded
numeric control/pane rectangles and viewport dimensions, never source text, DOM
identifiers, URLs or session data. The sample is cleared when the checkpoint changes.

| Playwright profile | Portrait | Landscape |
| --- | --- | --- |
| iPhone SE (3rd gen) | 375×667 | 667×375 |
| iPhone 13 | 390×664 | 750×342 |
| iPhone 14 Pro Max | 430×740 | 814×380 |
| iPad Mini | 768×1024 | 1024×768 |
| iPad Pro 11 | 834×1194 | 1194×834 |

These are viewport dimensions, **not physical screen resolutions**. Landscape
values are taken independently from the registry, not obtained by swapping the
portrait dimensions. Startup fails closed if they differ from the installed
pinned Playwright 1.64.1 registry. Sources:
[device descriptors](https://github.com/microsoft/playwright/blob/v1.64.1/packages/isomorphic/deviceDescriptorsSource.json)
and [Playwright emulation guidance](https://playwright.dev/docs/emulation).

This is sandboxed desktop Chromium **viewport-only** coverage. It does not claim
physical iPhone/iPad, Safari/WebKit, mobile user-agent, touch, device pixel ratio,
virtual keyboard, safe-area or browser-chrome emulation. The >800px cases test the
actual wider-pane breakpoint, including the 814px wide-phone landscape.

Each case verifies actual inner dimensions, breakpoint state, file navigation,
source-shape candidate navigation and focus, Outline symbol selection, return to
readable selected details, pane/keyboard focus, horizontal overflow, stable worker
count and zero additional Inspect requests. Short landscape heights may scroll
vertically; selected content must remain reachable and readable. Existing taller
viewport assertions are not weakened. No browser/runtime dependency is installed
by the matrix, and the normal fixed-suite deadline and cleanup policy still apply.


The `probe.report.stale_actions` group also deliberately queues an accepted real
WASM declaration-selection view, returns to the main pane before releasing it,
and then releases in FIFO order. It checks that external selection resets stale
restored focus/scroll while direct source-row actions retain their own focus.
This is controlled presentation latency, not a fabricated result or a new remote
request. The regular viewport matrix still exercises unsynchronized navigation.
