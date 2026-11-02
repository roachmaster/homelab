# State, event and transition contract

Part of the [workbench architecture](README.md). These contracts map to the
[model](../../../src/rust/forge-ide-ui/src/model.rs),
[protocol](../../../src/rust/forge-ide-ui/src/protocol.rs) and
[generated adapters](../../../src/rust/forge-ide-web/src/generate/javascript.rs).
Reference-style `EntryRef`/`SymbolRef` names are design shorthand; actual keyed
`UiEvent` fields are authoritative. Runtime/browser checks are separate from this
specification and its simplified diagrams.

## Separate three state domains

1. **Application state, existing WASM owner:** catalog, selected node/project,
   pending request, revision, accepted report, entry/declaration selections,
   expansions and row limits. Existing `Phase` is exactly `Unpaired`, `Pairing`,
   `Idle`, `Loading`, `Ready`, `Partial`, `Failed`, `Cancelled`.
2. **Presentation state, generated main-thread owner:** focus, scroll, DOM lifetime;
   pane geometry/visibility and active narrow pane. Changing geometry is
   not a report mutation or HTTP request. The main thread must not maintain a
   competing copy of application selection, filter or request policy.
3. **Transport/lifecycle state, worker adapter owner:** WASM availability, pending
   fetch controllers and session transport metadata. Fatal startup/ABI failure
   closes the worker adapter outside the eight application phases; it must not
   become a recoverable Inspect state on a broken instance.

Connection versus Workbench is a **composition of these states**. A report can be
partial while a user switches panes; no Cartesian-product enum such as
`MobileOutlinePartialLoadingPage` is needed.

## Request transitions and presentation

| Current condition + input | Guard and resulting state | Effect / display and focus contract |
| --- | --- | --- |
| Startup + `Resume` | No catalog or active pairing; new pairing request ID → `Pairing` | Existing Resume effect. Connection shows session check. No report/cached catalog is restored from browser storage. |
| Unpaired + `Pair { value }` | Bounded nonempty input, no concurrent pair/catalog → `Pairing` | Existing Pair effect; clear input after send. Focus stays in connection region; never echo code in status/logs. |
| Pairing + `Paired` | Matching pairing ID and valid bounded catalog → `Idle` | Select catalog's first node and its first project if any. Workbench composition appears. Focus moves to context selection after user pairing, without background focus stealing. |
| Pairing + `PairFailed` / matching network failure | Clear pairing → `Unpaired`; unmatched reply is ignored | Connection displays bounded safe failure. Retry/pair is explicit. An invalid catalog also stays unpaired. |
| Catalog accepted, selected node has no projects | Remain `Idle`; no valid project selection | Show “No allowed project”; Inspect disabled and model rejects invalid input. A catalog with no nodes is invalid, not an empty Workbench. No arbitrary path/URL field. |
| Workbench + valid node/project selection | Catalog membership checked; invalidate report/pending/selection, increment revision → `Idle` | Existing Abort if pending. Clear old data. Keep focus on the selector; pane geometry may remain but old row focus/filters cannot. |
| Idle/Ready/Partial/Failed/Cancelled + `Inspect` | Catalog/project valid, no pending request, valid bounded clock/expiry; invalidate old report, new token → `Loading` | Exactly one Inspect effect. Show selected context and Cancel local wait; no stale report or guessed percentage. |
| Loading + repeated `Inspect` | Existing `Busy` error; state/pending unchanged | No second effect. Disabled UI is not sufficient; retain model guard. Preserve current focus. |
| Loading + matching `Result::Report` | Token equality; protocol/node/project match; current time before expiry; full report validation → `Ready` or `Partial` | Accept complete/partial report, select root and expand it, clear pending. Announcements describe outcome without forcing focus away from a valid control. |
| Loading + mismatched token / no pending | Ignore stale result | No effect, no data replacement, no focus change. Different token is not repaired or treated as a new report. |
| Loading + matching token but bad target/project/version, expired or invalid report | Clear pending → `Failed` | Reject content. Explain invalid/late result; no source data displayed, no silent retry. Timeout cannot prove non-execution. |
| Loading + matching typed error or network failure | Clear pending → `Failed` | Denied/unavailable/timeout/limit/invalid-response category shown safely. Retry is a new explicit Inspect, not an effect replay. |
| Loading + `Cancel` | Invalidate, increment revision, remove pending/report → `Cancelled` | Existing Abort effect. “Stopped waiting locally”; agent work may continue. Subsequent old completion/failure ignored. |
| No pending + `Cancel` | Existing no-op | No request or state invention. Do not show an enabled Cancel as though work is pending. |
| Ready/Partial + select/toggle/more | IDs checked against current accepted report; local state changes only | Accepted-report keyed variants below. No backend calls. Files/fields/symbols are not filesystem navigation commands. |
| Fatal WASM/startup/ABI failure | Adapter closes, aborts local pending work; outside `Phase` | Recovery message and explicit Reload. No further dispatch into that instance. Remote completion remains unknown. |

Acceptance clears `pending` but now retains its token as `accepted_report`.
All local actions validate this context; new reports/context invalidation clear
it before another projection can become visible. This is a local ABI 2 change,
not a change to the native `ReportView` wire schema.

## Session expiry is not the same as operation denial

The current browser adapter maps HTTP 401/403 to `FailureKind::Denied`; the model
does not distinguish an expired host session from a denied operation/project in
that category. During inspection this becomes **Failed in Workbench**, not an
automatic Connection transition. Do not infer credentials, expiry or permission
changes from error prose. Keep the report cleared and offer an explicit **Reload
to check session** recovery (generated lifecycle control).

Reload creates a new worker and Resume checks the actual host session. A valid
catalog returns to idle Workbench; failed resume returns to Connection. It does
not automatically replay Inspect. A later distinct machine-readable session-expiry
reason would require a bounded protocol/adapter change and tests; this design
does not claim it exists. Pairing a new session is not an authority escalation or
credential provisioning workflow. Host and agent authorization remain separate.

## Local view transitions — implemented keyed actions

| Intent | Validation and local change | Focus / effect |
| --- | --- | --- |
| `SelectEntry(EntryRef)` | Accepted report key matches; entry exists. Select it, clear symbol and symbol filter. | Show complete safe name/metadata; no network. Explicit activation may focus file heading, but background rendering must not. |
| `SelectSymbol(SymbolRef)` | Key/entry/declaration exist and belong together. Select entry + symbol. | Show structural details; no source fetch, editor or inferred signature. |
| `SelectProblem(ProblemRef)` | Key/entry/diagnostic index exist. Select entry, clear symbol, reveal supplied string. | Explicitly expose file details/diagnostic; no invented source location or remote effect. Keep Problems context reversible by pane controls. |
| Set/clear file or symbol filter | Correct report/entry key, bounded query. Compute against full accepted scope before paging. | WASM projection only. Preserve selection with “hidden by filter” state where needed; no remote search or automatic request. |
| Previous/next rows | Matching accepted context; move the bounded local window | Keep focus on the page control consistently; announce the displayed range. No new inspection. |
| Hide/show/resize/reset pane | Typed declared pane ID and bounded geometry; main presentation only | Retain valid semantic selection. Move focus from a hidden subtree to its reopen control; never leave hidden tabbable rows. |
| Narrow pane switch | Target pane exists; update active presentation region | Restore last valid local focus in that pane or its heading. Do not remount the worker or discard accepted report. |
| Old row/problem/filter/toggle/pagination event after new report | Report key differs, or IDs no longer valid; outline paging/filtering must also match selected entry | Reject/no-op without effects or selection/expansion/window change. A reused integer ID must not affect a different report's file. |

For source names/spans and partial/binary/unreadable conditions, use actual typed
data. The new Problems projection cannot parse diagnostic strings to fabricate
severity or locations. Filtering and counts disclose that excluded/partial entries
are outside analyzed content. Explicitly unsupported Rust syntax is not “no code”.

## Browser lifecycle and keyboard flow

- **Mount / pageshow:** start one worker, validate real ABI, send Resume once.
  Repeated mount signals must not create duplicate workers/listeners/requests.
- **Pagehide / refresh / browser Back:** terminate the local worker and release
  browser ownership. This is not a remote cancellation receipt. On return, obtain
  a fresh catalog and no report; no URL/history entry restores a stale report ID.
  Generated lifecycle primitives clear protected catalog/report DOM on teardown
  and before a fresh resume, including browser-cache restoration. Fatal handling
  clears/disables content and offers explicit fresh-instance recovery; it does not
  assume a replacement worker instantly erases stale pixels.

- **Current session, same report:** pane toggling preserves model selection;
  presentation stores only bounded valid focus/scroll references. New report or
  context discards old focus keys. No persistence of pairing/CSRF/report in URLs.
- **Keyboard path:** labeled context controls → Explorer → selected file workspace
  → optional Outline/Problems → view controls. Native Tab order plus tree arrow
  navigation; Enter activates selection, not remote Inspect. Up/Down focus alone
  does not select a different file; Left/Right tree hierarchy behavior follows visible children/parents as well as expansion.
- **Resize:** focused separators expose name, range/current value and keyboard
  adjustment/reset; pointer drag is not the sole interaction. Hidden panes have
  visible controls to reopen. Do not intercept browser Back/refresh/zoom shortcuts.
- **Result arrival:** update accepted view, preserve still-valid focus; if its
  target disappeared, move to a stable visible control/heading. Announce completion
  or failure once, not every tree row. Long/native-encoded names stay safe text and
  fully available through selected-item details, not exclusively pointer hover.

## Acceptance scenarios and implementation gates

| Scenario | Required observable result |
| --- | --- |
| Start with valid/absent/expired session | Exactly one Resume; valid catalog → Idle, failed resume → Connection; no old report or leaked secret |
| Select project B while A loads | Abort A locally, increment revision and clear report. Late A result/network failure cannot populate B or move focus |
| Repeat Inspect / retry a failure | One in-flight request; explicit retry gets a new token/deadline and no automatic retry loop |
| Complete empty root / valid file with no declarations | Accurate empty-result messages, distinct from not inspected, binary, unreadable and parse-unavailable states |
| Partial report with multiple diagnostics | Usable entries remain available; Problems count/index is scoped to this report, without compiler severity/location invention |
| Timeout / unavailable / denied / Cancel | Correct uncertainty and recovery; local Cancel never claims agent termination; generic Denied never assumes expiry |
| Old row click/toggle/more after a new inspection reuses ID 0 | Report-key mismatch blocks all local changes; old outline paging also cannot affect a newly selected file; no backend call or cross-report data presentation |
| Filter a long/native-encoded name then clear | Matching follows documented safe-name rules, parent context retained, no hidden backend search; complete name accessible |
| Select a problem, symbol, collapse Outline, resize, narrow/widen | Same report ownership; correct entry/symbol state and reversible panes; no hidden focus targets or duplicate worker |
| Refresh/browser leave-return after success or while loading | Old worker torn down; new Resume only; no automatic inspect, report cache or stale history restoration |
| Invalid/oversized message or fatal ABI result | Bounded rejection; fatal instance stops; explicit recovery only; no unsafe markup, new URL or admission bypass |

Run pure model and generator/non-IDE fixture tests first, then actual generated
browser + WASM with real host/agent plus separately identified fault fixtures.
Static review of these tables/diagrams is not execution of those scenarios.

[Component contracts](components.md) · [Overview and stages](README.md)
