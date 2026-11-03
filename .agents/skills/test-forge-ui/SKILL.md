---
name: test-forge-ui
description: Test or review the generated Forge IDE in a real browser, reuse its headless acceptance suite, and report visual, keyboard, request-lifetime and admission evidence. Use for Forge UI QA or regression verification, not browser setup or generic website automation.
---

# Test Forge UI

Test the actual generated application and real WASM, not a handwritten substitute.
The repository already has a sandboxed headless Chrome suite and structured reports.
Read its [current guide](../../../src/rust/forge-ide-web/tests/browser/README.md)
before running or extending it; its catalog and evidence scopes are authoritative.
This skill adapts [Playwright MCP guidance](../../../docs/design/skill-sources.md)
to that existing workflow. It installs no server, extension, browser or service.

## Choose the evidence needed

- **Document/skill-only change:** validate links, metadata and claimed APIs/commands.
  Report that no application was changed and browser execution was not required.
- **Model, generator or adapter change:** run the applicable native/schema/ABI and
  generation checks, plus actual browser acceptance when behavior or rendering is
  affected. `make ui-check` and `make ui-qa-check` are useful but neither proves
  browser-to-agent acceptance. Do not treat a focused run as the full gate.
- **Full workbench acceptance:** use `make ui-qa` from the repository root with the
  documented prerequisites already present. It rebuilds atomic assets/WASM, uses
  owned localhost fixtures and ephemeral identities, and runs the fixed ordered
  suite. Do not narrow it with package flags or replace real-agent cases with mocks.
- **Exploratory investigation:** use an already available, authorized Playwright
  browser tool against a test-owned local instance when useful. It supplements the
  reproducible gate; successful manual exploration does not replace the suite.

If a prerequisite or sandbox blocks a run, report the exact failed stage and what
remains unrun. Do not auto-install packages, run an upstream launcher, add MCP
configuration, start an exposed server or change host security as a fallback.

## Explore without changing the architecture

Use fresh page/accessibility snapshots to identify current controls, and target
their roles, accessible names or current element references rather than guessing
coordinates or reusing stale references after a render. Check screenshots as well:
an accessibility snapshot cannot reveal clipping, spacing or visual focus quality.
Verify visible state and relevant effects after an action, not just click success.

Exercise meaningful paths for the requested change: keyboard focus versus selection,
tree/filter/pagination and pane controls, desktop/narrow layouts, empty/partial/
denied reports, long/HTML-like text, rapid repeated actions, late responses, local
cancel/retry, refresh and browser leave/return. Check that report-local actions do
not create inspection requests and obsolete report/file events cannot act on the
current view. Preserve session/revision/request/report guards and fail-closed DOM
clearing. Do not claim these paths passed if they were only inspected statically.

Keep the app's ownership split: YAML/Rust declarations generate web assets;
worker/WASM owns application policy; generated I/O glue uses admitted effects;
host and remote agent authorize independently. Test-driver JavaScript is permitted
test code, not permission to hand-author application JS. A fix belongs in the
declaration, Rust model or reusable generator/adapter primitive responsible for it.

## Keep browser evidence bounded and private

Use only disposable fixtures and test-owned data for this suite. For optional MCP
exploration, require an isolated test context/profile, no real logged-in browser,
personal storage state or extension attachment. If the available tool cannot
provide that context, use the existing harness or report the limitation. Origin
allow/block lists are guardrails, not an authorization or isolation boundary.
Never enable unrestricted file access, disable the browser sandbox, ignore TLS
errors or expose a browser debugging port to make a test pass.

Follow the harness's artifact allowlist and masking rules. Do not dump credentials,
pairing/session tokens, source reports, raw console/network output, browser storage,
traces or fixture logs into reports or attachments. Inspect screenshots before
sharing; capture only known safe fixture states. Close only resources owned by the
run, and report cleanup failure separately from the original test failure.

## Report what actually ran

The current suite has 18 ordered groups. Read fresh `QA_OUTPUT` reports and inspect
its generated screenshots. A stopped sequence leaves later groups `not_run`, not
passed. Keep real-agent, actual-WASM probes and controlled fault injection labeled
separately. Local cancellation does not prove remote task termination; a synthetic
page-lifetime event does not prove real browser bfcache use.

Record revision and dirty/source fingerprint, command, stage/scenario outcomes,
screenshot findings, cleanup and remaining gaps. Do not reuse old run directories
as current evidence. If publishing a tested change is authorized, verify the exact
remote commit and its applicable CI separately from local results. Invocation does
not itself authorize fixes, publication, deployment or environment setup.
