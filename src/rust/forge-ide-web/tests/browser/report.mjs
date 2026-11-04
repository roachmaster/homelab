// Test-harness reporting only. No raw exception, browser/network, fixture output,
// URL, filesystem path, storage or credential values cross this boundary.
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { SCENARIOS, SUITE_DESCRIPTION } from './catalog.mjs';

const STAGES = new Set(['arguments', 'dependency', 'input_validation', 'fingerprints', 'browser_channel', 'browser_launch', 'fixture_start', 'context_start', 'scenario', 'diagnostics', 'cleanup', 'completion', 'interrupted', 'deadline']);
const CATEGORIES = Object.freeze({
  arguments: 'invalid_arguments', dependency: 'browser_dependency_unavailable',
  input_validation: 'input_validation_failed', fingerprints: 'input_fingerprinting_failed',
  browser_channel: 'unsupported_browser_channel', browser_launch: 'browser_launch_failed',
  fixture_start: 'fixture_start_failed', context_start: 'context_start_failed',
  scenario: 'assertion_or_runtime_failure', completion: 'incomplete_suite',
  interrupted: 'interrupted', deadline: 'run_deadline_exceeded',
});
const CLEANUP_NAMES = new Set(['browser', 'real_fixture', 'injected_fixture']);
const DIAGNOSTICS = new Set(['disabled', 'captured', 'unavailable', 'skipped_unsafe_page', 'skipped_credential_pattern']);
// Static test-driver checkpoints, never page-supplied names or DOM identifiers.
const MATRIX_CHECKPOINTS = new Set([
  'viewport_resize', 'viewport_dimensions', 'viewport_breakpoint',
  'explorer_activate', 'file_filter', 'file_select', 'inventory_activate',
  'inventory_caveat', 'candidate_select', 'candidate_focus', 'candidate_selection',
  'outline_activate', 'symbol_select', 'symbol_focus', 'details_activate',
  'details_content', 'details_scroll', 'details_readable', 'outline_return',
  'outline_restore', 'filter_keyboard_exit', 'symbol_keyboard_return',
  'workspace_return', 'horizontal_overflow', 'worker_lifetime', 'local_requests', 'viewport_screenshot',
]);
function numericRecord(value, keys, minimum) {
  if (!value || typeof value !== 'object' || Object.getPrototypeOf(value) !== Object.prototype) throw new Error('Invalid geometry');
  const descriptors = Object.getOwnPropertyDescriptors(value);
  if (Reflect.ownKeys(descriptors).length !== keys.length || keys.some(key => !Object.hasOwn(descriptors, key))) throw new Error('Invalid geometry');
  const result = {};
  for (const key of keys) {
    const descriptor = descriptors[key];
    const number = descriptor.value;
    if (!Object.hasOwn(descriptor, 'value') || typeof number !== 'number' || !Number.isFinite(number) || number < minimum || number > 100000) throw new Error('Invalid geometry');
    result[key] = number;
  }
  return result;
}
function safeFocusGeometry(value) {
  if (!value || typeof value !== 'object' || Object.getPrototypeOf(value) !== Object.prototype) throw new Error('Invalid geometry');
  const fields = Object.getOwnPropertyDescriptors(value);
  const keys = ['focused', 'pane', 'viewport'];
  if (Reflect.ownKeys(fields).length !== keys.length || keys.some(key => !Object.hasOwn(fields, key) || !Object.hasOwn(fields[key], 'value'))) throw new Error('Invalid geometry');
  const rectangle = value => numericRecord(value, ['left', 'top', 'right', 'bottom'], -100000);
  return { focused: rectangle(fields.focused.value), pane: fields.pane.value === null ? null : rectangle(fields.pane.value), viewport: numericRecord(fields.viewport.value, ['width', 'height'], 0) };
}

const timestamp = () => new Date().toISOString();
const elapsed = start => Math.max(0, Math.round(performance.now() - start));

function metadata(env) {
  const result = { node: process.versions.node, browser_channel: 'chrome', chromium_sandbox: true };
  if (/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(env.FORGE_UI_QA_REVISION ?? '')) result.revision = env.FORGE_UI_QA_REVISION;
  if (/^[0-9a-f]{64}$/.test(env.FORGE_UI_QA_SOURCE_SHA256 ?? '')) result.source_sha256 = env.FORGE_UI_QA_SOURCE_SHA256;
  if (['0', '1', 'true', 'false', 'unknown'].includes(env.FORGE_UI_QA_DIRTY)) result.dirty = env.FORGE_UI_QA_DIRTY;
  return result;
}

export class Report {
  constructor(env = {}) {
    this.started = performance.now();
    this.stage = 'arguments';
    this.active = null;
    this.frozen = false;
    this.data = {
      schema_version: 1, status: 'running', description: SUITE_DESCRIPTION,
      started_at: timestamp(), finished_at: null, duration_ms: null,
      metadata: metadata(env), failure: null, cleanup: [],
      failure_screenshot: 'disabled',
      scenarios: SCENARIOS.map(item => ({ ...item, status: 'not_run', started_at: null, duration_ms: null })),
    };
  }
  ensureActive() {
    if (this.frozen) throw new Error('Journey has stopped');
  }
  setStage(stage) {
    if (!STAGES.has(stage)) throw new Error('Unknown reporting stage');
    if (!this.frozen) this.stage = stage;
  }
  start(id) {
    if (this.frozen) throw new Error('Journey has stopped');
    const item = this.data.scenarios.find(item => item.id === id);
    if (!item || this.active || item.status !== 'not_run' ||
        this.data.scenarios.slice(0, item.order - 1).some(prior => prior.status !== 'passed')) {
      throw new Error('Invalid sequential scenario transition');
    }
    this.stage = 'scenario';
    this.active = item;
    this.scenarioStarted = performance.now();
    item.started_at = timestamp();
    // The final artifact has only passed, failed and not_run scenario statuses.
    // While running, started_at distinguishes the active not-yet-completed group.
  }
  checkpoint(name) {
    if (this.frozen || this.data.failure) return;
    if (!this.active?.viewport || !MATRIX_CHECKPOINTS.has(name)) throw new Error('Invalid matrix checkpoint');
    if (this.active.checkpoint !== name) delete this.active.focus_geometry;
    this.active.checkpoint = name;
  }
  focusGeometry(scenario, checkpoint, value) {
    if (this.frozen || this.data.failure || !this.active || this.active.id !== scenario || this.active?.checkpoint !== checkpoint) return;
    if (!this.active.viewport || !MATRIX_CHECKPOINTS.has(checkpoint)) throw new Error('Invalid matrix checkpoint');
    this.active.focus_geometry = safeFocusGeometry(value);
  }
  pass(id) {
    if (this.frozen || this.active?.id !== id) throw new Error('Invalid scenario completion');
    this.active.status = 'passed';
    this.active.duration_ms = elapsed(this.scenarioStarted);
    this.active = null;
  }
  fail(stage = this.stage) {
    if (this.data.failure) return; // preserve the primary failure through teardown
    const safeStage = STAGES.has(stage) ? stage : 'scenario';
    this.data.failure = { stage: safeStage, category: CATEGORIES[safeStage] ?? 'harness_failure', scenario_id: this.active?.id ?? null, ...(this.active?.checkpoint ? { checkpoint: this.active.checkpoint } : {}) };
    if (this.active) {
      this.active.status = 'failed';
      this.active.duration_ms = elapsed(this.scenarioStarted);
    }
  }
  addFingerprints(values) {
    const allowed = ['index.html', 'style.css', 'main.js', 'worker.js', 'app.wasm', 'manifest.json', 'agent_binary', 'fixture_binary'];
    const result = {};
    for (const name of allowed) if (/^[0-9a-f]{64}$/.test(values[name] ?? '')) result[name] = values[name];
    this.data.metadata.fingerprints_sha256 = result;
  }
  browserVersion(value) {
    if (/^\d+(?:\.\d+){0,3}$/.test(value)) this.data.metadata.browser_version = value;
  }
  cleanup(name, status, duration) {
    if (!CLEANUP_NAMES.has(name) || !['passed', 'failed'].includes(status)) throw new Error('Unknown cleanup record');
    const existing = this.data.cleanup.find(item => item.name === name);
    const record = { name, status, duration_ms: Math.max(0, Math.round(duration)), ...(status === 'failed' ? { category: 'cleanup_failed' } : {}) };
    if (existing) { if (status === 'failed') Object.assign(existing, record); }
    else if (this.data.cleanup.length < 3) this.data.cleanup.push(record);
  }
  screenshot(status) {
    if (!DIAGNOSTICS.has(status)) throw new Error('Unknown screenshot status');
    this.data.failure_screenshot = status;
  }
  finish() {
    if (!this.data.failure && !this.data.cleanup.some(item => item.status === 'failed') && this.data.scenarios.some(item => item.status !== 'passed')) this.fail('completion');
    this.frozen = true;
    this.data.finished_at = timestamp();
    this.data.duration_ms = elapsed(this.started);
    this.data.counts = Object.fromEntries(['passed', 'failed', 'not_run'].map(status => [status, this.data.scenarios.filter(item => item.status === status).length]));
    this.data.status = this.data.failure || this.data.cleanup.some(item => item.status === 'failed') ? 'failed' : 'passed';
    return this.data;
  }
}

export async function bounded(operation, timeoutMs) {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Bounded operation deadline')), timeoutMs); }),
    ]);
  } finally { clearTimeout(timer); }
}

// Track asynchronous acquisition BEFORE awaiting it. Shutdown waits for the
// resource and disposes it even if acquisition completes after cancellation. A
// bounded cleanup timeout records failure, never a false clean shutdown; the
// disposal promise remains attached so a late resolution still closes the owner.
export function acquireOwned(open, close) {
  let closing = false;
  let disposal;
  const ready = Promise.resolve().then(() => {
    if (closing) throw new Error('Acquisition cancelled before opening');
    return open();
  });
  return {
    ready,
    close() {
      closing = true;
      disposal ??= ready.then(resource => close(resource), () => {});
      return disposal;
    },
  };
}

// Every registered resource gets an independent bounded attempt, even if an
// earlier one rejects/hangs. Callers pass only static resource names.
export async function cleanupResources(resources, report, timeoutMs = 15_000) {
  for (const { name, close } of resources) {
    const start = performance.now();
    try { await bounded(close, timeoutMs); report.cleanup(name, 'passed', performance.now() - start); }
    catch { report.cleanup(name, 'failed', performance.now() - start); }
  }
}

const xml = value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&apos;');
export function summary(data) {
  const rows = data.scenarios.map(item => `| ${item.order} | ${item.id} | ${item.scope} / ${item.backend} | ${item.viewport ? `${item.viewport.profile} ${item.viewport.width}×${item.viewport.height}` : '-'} | ${item.status} | ${item.duration_ms ?? '-'} |`);
  const primary = data.failure ? `\nFailure: **${data.failure.category}**, stage **${data.failure.stage}**, group **${data.failure.scenario_id ?? 'none'}**${data.failure.checkpoint ? `, checkpoint **${data.failure.checkpoint}**` : ''}.\n` : '';
  const geometry = data.scenarios.find(item => item.status === 'failed')?.focus_geometry;
  const geometryText = geometry ? `\nBounded CSS-pixel focus geometry: ${JSON.stringify(geometry)}\n` : '';
  const cleanup = data.cleanup.map(item => `- ${item.name}: ${item.status} (${item.duration_ms} ms)`).join('\n') || '- No resource was started.';
  return `# Browser QA: ${data.status}\n\n${SUITE_DESCRIPTION}\n\nPassed: ${data.counts?.passed ?? 0}; failed: ${data.counts?.failed ?? 0}; not run: ${data.counts?.not_run ?? SCENARIOS.length}.\n${primary}${geometryText}\n| # | Group | Scope / backend | Viewport | Result | ms |\n| --- | --- | --- | --- | --- | ---: |\n${rows.join('\n')}\n\n## Cleanup\n${cleanup}\n\nFailure screenshot: ${data.failure_screenshot}. Screenshots are fixture-only evidence, not an accessibility certification. No raw exception or browser/network/fixture logs are retained.\n`;
}
export function junit(data) {
  const cases = data.scenarios.map(item => {
    const content = item.status === 'failed' ? `<failure type="${xml(data.failure?.category ?? 'harness_failure')}" message="${xml((data.failure?.stage ?? 'scenario') + (item.checkpoint ? ': ' + item.checkpoint : ''))}"/>` : item.status === 'not_run' ? '<skipped message="Not reached in sequential journey"/>' : '';
    return `  <testcase classname="forge.browser.${item.scope}" name="${xml(item.id)}" time="${(item.duration_ms ?? 0) / 1000}">${content}${item.status === 'failed' && item.focus_geometry ? `<system-out>${xml(JSON.stringify(item.focus_geometry))}</system-out>` : ''}</testcase>`;
  });
  // Harness failure is not an additional acceptance group. A separate suite makes
  // launch/cleanup failures visible to CI even if all catalog groups passed/not_run.
  const harnessFailure = data.failure && !data.scenarios.some(item => item.status === 'failed');
  const cleanupFailures = data.cleanup.filter(item => item.status === 'failed');
  const errors = [...(harnessFailure ? [`${data.failure.stage}: ${data.failure.category}`] : []), ...cleanupFailures.map(item => `${item.name}: cleanup_failed`)];
  const harness = errors.length ? `\n<testsuite name="forge.browser.harness" tests="1" failures="0" errors="1"><testcase name="harness"><error type="harness_failure" message="${xml(errors.join('; '))}"/></testcase></testsuite>` : '';
  return `<?xml version="1.0" encoding="UTF-8"?>\n<testsuites>\n<testsuite name="forge.browser.acceptance" tests="${SCENARIOS.length}" failures="${data.counts.failed}" errors="0" skipped="${data.counts.not_run}" time="${data.duration_ms / 1000}">\n${cases.join('\n')}\n</testsuite>${harness}\n</testsuites>\n`;
}
export async function writeReport(output, data) {
  await writeFile(join(output, 'result.json'), `${JSON.stringify(data, null, 2)}\n`, { mode: 0o600 });
  await writeFile(join(output, 'summary.md'), summary(data), { mode: 0o600 });
  await writeFile(join(output, 'junit.xml'), junit(data), { mode: 0o600 });
}

// Dependency injection tests exercise orchestration/reporting, never pretend to
// be E2E evidence. Setup happens only AFTER exclusive output reservation.
export async function runHarness({ args, env = process.env, setup, journey, cleanup, diagnose, log = line => console.log(line), deadlineMs = 180_000, signalSource = process }) {
  const report = new Report(env);
  const output = args[3];
  if (!output) { log('FAIL: invalid_arguments (output directory required).'); return false; }
  try { await mkdir(output, { mode: 0o700 }); }
  catch { log('FAIL: output_directory_unavailable (requires a new directory; existing evidence was not changed).'); return false; }
  let timer;
  let interrupt;
  const interrupted = new Promise((_, reject) => { interrupt = stage => { report.fail(stage); reject(new Error('Journey interrupted')); }; });
  const onSignal = () => interrupt('interrupted');
  signalSource.on('SIGINT', onSignal);
  signalSource.on('SIGTERM', onSignal);
  timer = setTimeout(() => interrupt('deadline'), deadlineMs);
  try {
    await Promise.race([(async () => {
      if (args.length !== 4) throw new Error('Invalid arguments');
      await setup(report, output);
      await journey(report, output);
    })(), interrupted]);
  } catch {
    report.fail();
    report.frozen = true;
    if (diagnose) {
      try { await bounded(() => diagnose(report, output), 5_000); }
      catch { report.screenshot('unavailable'); }
    }
  } finally {
    clearTimeout(timer);
    report.frozen = true;
    try { await cleanup(report); }
    catch { report.cleanup('browser', 'failed', 0); }
    signalSource.off('SIGINT', onSignal);
    signalSource.off('SIGTERM', onSignal);
  }
  const data = report.finish();
  try { await writeReport(output, data); }
  catch { log('FAIL: report_write_failed.'); return false; }
  log(`${data.status === 'passed' ? 'PASS' : 'FAIL'}: ${data.counts.passed}/${SCENARIOS.length} ordered browser acceptance groups; ${data.counts.failed} failed; ${data.counts.not_run} not run. Cleanup: ${data.cleanup.some(item => item.status === 'failed') ? 'failed' : 'passed'}.`);
  return data.status === 'passed';
}
