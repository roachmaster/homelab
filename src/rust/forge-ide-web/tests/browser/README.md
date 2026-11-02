# Workbench browser acceptance

`journey.mjs` drives the generated application with the real Rust WASM ABI. Run it
only with an installed, sandbox-capable browser and newly built matching assets,
agent and fixture binaries:

```sh
FORGE_UI_BROWSER_CHANNEL=chrome node journey.mjs \
  /absolute/generated-assets /absolute/forge-agent \
  /absolute/loopback_fixture /absolute/owned-output
```

The pinned Playwright dependency is in this directory; `npm ci` preserves its
lockfile. Chromium sandboxing is always enabled. Do not add `--no-sandbox`, ignore
certificate validation, alter host sandbox policy or substitute a fake WASM build
when an environment cannot run this gate. A syntax check or native test is not a
browser pass.

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

Successful runs write `result.json` with scoped check descriptions and desktop,
narrow, empty/partial/denied and controlled-state PNGs. A failed run records only
completed groups; it is never relabeled passed. Pairing input is cleared before
screenshots and masked. Do not enable traces, request-body dumps or credential-
bearing fixture output. Screenshots contain only disposable fixture project data.
The temporary test process groups and owned source/key directories are cleaned up.

## Focused checks (not browser evidence)

```sh
node --check journey.mjs
cargo test --manifest-path ../../../Cargo.toml \
  -p forge-ide-web-host --example loopback_fixture --locked
```

The native fixture tests validate that the exact real-agent source corpus projects
to a bounded portable report with >256 rows, a beyond-page match, valid empty text,
binary/parse-unavailable distinctions, encoded names and a complete empty root.
They also check the separately injected port's response context/error categories.
Hosted browser execution and visual review of all generated screenshots remain a
separate required acceptance gate.
