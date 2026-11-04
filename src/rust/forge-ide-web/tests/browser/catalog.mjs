import { VIEWPORTS } from './viewports.mjs';
// Ordered acceptance groups, not independently selectable test cases. Later groups
// reuse accepted reports, browser state and fixtures established by earlier groups.
const definitions = [
  ['real.report.partial', 'real', 'real_mtls', 'Partial structural report through generated UI, WASM, host and mTLS agent'],
  ['real.navigation.keyboard', 'real', 'real_mtls', 'Tree keyboard focus is independent of selection'],
  ['real.filters.safe_text', 'real', 'real_mtls', 'Local full-report filters, bounded input and safe complete names'],
  ['real.pagination.problems', 'real', 'real_mtls', 'Bounded reversible pagination and truthful file/problem states without I/O'],
  ['real.panes.desktop', 'real', 'real_mtls', 'Desktop pointer/keyboard pane sizing, bounds, reset and focus'],
  ['real.panes.narrow', 'real', 'real_mtls', 'Narrow viewport pane navigation and widening preserve context'],
  ...VIEWPORTS.map(item => [item.id, 'real', 'real_mtls', `${item.profile} ${item.width}×${item.height} CSS-pixel Chromium viewport navigation, focus and overflow`]),
  ['probe.report.stale_actions', 'lifecycle_probe', 'real_mtls', 'Real WASM rejects stale actions; FIFO-delayed external selection resets stale restored pane focus'],
  ['probe.page.lifecycle', 'lifecycle_probe', 'real_mtls', 'Synthetic page transitions plus real reload and history clear protected DOM'],
  ['real.report.empty', 'real', 'real_mtls', 'Complete empty project retains its root and truthful diagnostics'],
  ['real.admission.denied', 'real', 'real_mtls', 'Independent real agent project denial clears report with explicit recovery'],
  ['injected.requests.cancel_retry', 'injected', 'injected_port', 'Cancellation, late completion, repeated Inspect guard and explicit retry'],
  ['probe.presentation.ordered_drafts', 'lifecycle_probe', 'injected_port', 'Controlled FIFO real-WASM view latency preserves current input drafts'],
  ['injected.requests.revisions', 'injected', 'injected_port', 'Rapid project revisions invalidate pending work without automatic retry'],
  ['injected.requests.refresh_focus', 'injected', 'injected_port', 'Refresh during pending work and background completion preserve valid focus'],
  ['injected.failures.unavailable_timeout', 'injected', 'injected_port', 'Unavailable and non-preemptible timeout stay explicit recoverable failures'],
  ['probe.worker.fatal', 'lifecycle_probe', 'injected_port', 'Controlled generated-worker fatal routine clears and disables protected DOM'],
  ['injected.catalog.empty', 'injected', 'catalog_response_override', 'Catalog-only empty allowed-project presentation, not authorization evidence'],
  ['probe.session.missing', 'lifecycle_probe', 'injected_port', 'Explicit cookie clearing and reload return to Connection without protected DOM'],
];
export const SCENARIOS = Object.freeze(definitions.map(([id, scope, backend, title], index) => Object.freeze({
  id, order: index + 1, scope, backend, title,
  depends_on: index ? definitions[index - 1][0] : null,
  ...(VIEWPORTS.some(item => item.id === id) ? { viewport: VIEWPORTS.find(item => item.id === id) } : {}),
})));
export const SUITE_DESCRIPTION = '28 ordered acceptance groups in one sequential journey; groups share state and are not independently selectable. Ten device-profile dimensions are Chromium viewport-only coverage, not physical iOS, WebKit, touch, safe-area or browser-chrome emulation. Real scope uses the generated UI, executable Rust WASM, native host and real mTLS agent. Injected scope is controlled fault/presentation evidence, not remote transport E2E evidence. Lifecycle probes explicitly manipulate events, views, worker lifecycle or session state.';
