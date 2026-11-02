// These dependency-injected Node tests prove harness behavior, NOT browser E2E.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { EventEmitter, once } from 'node:events';
import { spawn } from 'node:child_process';
import { SCENARIOS } from './catalog.mjs';
import { captureFailureScreenshot, SCREENSHOT_MASK } from './diagnostics.mjs';
import { requireRunningFixtureLeader } from './fixture-lifecycle.mjs';
import { Report, acquireOwned, cleanupResources, runHarness } from './report.mjs';

const SECRET = 'password=do-not-record Bearer arbitrary-secret cookie=session-secret PAIRING ' + 'a'.repeat(64) + '\n-----BEGIN PRIVATE KEY-----\nprivate-key-content';
async function workspace(t) {
  const root = await mkdtemp(join(tmpdir(), 'forge-report-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}
async function artifacts(output) {
  return Promise.all(['result.json', 'summary.md', 'junit.xml'].map(name => readFile(join(output, name), 'utf8')));
}
function defaults(output, options = {}) {
  return { args: ['assets', 'agent', 'fixture', output], env: {}, signalSource: new EventEmitter(),
    setup: async () => {}, journey: async () => {}, cleanup: async () => {}, log: () => {}, ...options };
}
function complete(report) {
  for (const item of SCENARIOS) { report.start(item.id); report.pass(item.id); }
}

test('catalog is exactly 18 unique ordered groups with honest sequential dependencies and scope labels', () => {
  assert.equal(SCENARIOS.length, 18);
  assert.equal(new Set(SCENARIOS.map(item => item.id)).size, 18);
  for (const [index, item] of SCENARIOS.entries()) {
    assert.equal(item.order, index + 1);
    assert.equal(item.depends_on, SCENARIOS[index - 1]?.id ?? null);
    assert.ok(['real', 'injected', 'lifecycle_probe'].includes(item.scope));
  }
  const report = new Report();
  assert.throws(() => report.start(SCENARIOS[1].id));
});

test('passed report contains all timings, scope labels, summary and 18 JUnit cases', async t => {
  const output = join(await workspace(t), 'browser');
  const passed = await runHarness(defaults(output, { journey: complete }));
  assert.equal(passed, true);
  const [json, summary, xml] = await artifacts(output);
  const data = JSON.parse(json);
  assert.equal(data.status, 'passed');
  assert.deepEqual(data.counts, { passed: 18, failed: 0, not_run: 0 });
  assert.ok(data.scenarios.every(item => item.started_at && item.duration_ms >= 0));
  assert.match(summary, /not independently selectable/);
  assert.equal((xml.match(/<testcase /g) ?? []).length, 18);
  assert.match(xml, /tests="18" failures="0"/);
});

test('scenario failure preserves passed and not_run records without serializing adversarial errors', async t => {
  const output = join(await workspace(t), 'browser');
  const logs = [];
  const passed = await runHarness(defaults(output, {
    env: { FORGE_UI_QA_REVISION: SECRET, FORGE_UI_QA_SOURCE_SHA256: SECRET, FORGE_UI_QA_DIRTY: SECRET },
    log: line => logs.push(line),
    journey(report) {
      report.start(SCENARIOS[0].id); report.pass(SCENARIOS[0].id);
      report.start(SCENARIOS[1].id);
      const error = new Error(SECRET); error.stack = SECRET;
      Object.defineProperty(error, 'message', { get() { throw new Error('Exception message must not be inspected'); } });
      throw error;
    },
  }));
  assert.equal(passed, false);
  const texts = await artifacts(output);
  const data = JSON.parse(texts[0]);
  assert.deepEqual(data.counts, { passed: 1, failed: 1, not_run: 16 });
  assert.deepEqual(data.failure, { stage: 'scenario', category: 'assertion_or_runtime_failure', scenario_id: SCENARIOS[1].id });
  assert.equal(data.scenarios[2].started_at, null);
  for (const text of [...texts, ...logs]) {
    for (const secret of ['do-not-record', 'arbitrary-secret', 'session-secret', 'a'.repeat(64), 'private-key-content']) assert.equal(text.includes(secret), false);
  }
});

test('all prelaunch stages create failed reports with 18 not_run groups', async t => {
  const root = await workspace(t);
  for (const stage of ['arguments', 'dependency', 'input_validation', 'fingerprints', 'browser_channel', 'browser_launch']) {
    const output = join(root, stage);
    let journeyRan = false;
    assert.equal(await runHarness(defaults(output, {
      setup(report) { report.setStage(stage); throw new Error(SECRET); },
      journey() { journeyRan = true; },
    })), false);
    const [json, , xml] = await artifacts(output);
    const data = JSON.parse(json);
    assert.equal(journeyRan, false);
    assert.equal(data.failure.stage, stage);
    assert.deepEqual(data.counts, { passed: 0, failed: 0, not_run: 18 });
    assert.match(xml, /name="forge.browser.harness"/);
  }
});

test('invalid arity with known output directory reports before setup; missing output logs only static text', async t => {
  const output = join(await workspace(t), 'browser');
  let setupRan = false;
  assert.equal(await runHarness(defaults(output, { args: ['a', 'b', 'c', output, SECRET], setup() { setupRan = true; } })), false);
  assert.equal(setupRan, false);
  assert.equal(JSON.parse((await artifacts(output))[0]).failure.stage, 'arguments');
  const logs = [];
  assert.equal(await runHarness(defaults(undefined, { args: [SECRET], log: line => logs.push(line) })), false);
  assert.equal(logs.join('').includes(SECRET), false);
});

test('existing output directory is never overwritten or mistaken for a current pass', async t => {
  const output = join(await workspace(t), 'browser');
  assert.equal(await runHarness(defaults(output, { journey: complete })), true);
  await writeFile(join(output, 'sentinel'), SECRET);
  const before = await artifacts(output);
  let setupRan = false;
  const logs = [];
  assert.equal(await runHarness(defaults(output, { setup() { setupRan = true; }, log: line => logs.push(line) })), false);
  assert.equal(setupRan, false);
  assert.deepEqual(await artifacts(output), before);
  assert.equal(await readFile(join(output, 'sentinel'), 'utf8'), SECRET);
  assert.match(logs.join(''), /requires a new directory/);
});

test('cleanup independently attempts all resources, bounds hangs, preserves primary failure', async t => {
  const output = join(await workspace(t), 'browser');
  const attempted = [];
  const passed = await runHarness(defaults(output, {
    journey(report) { report.start(SCENARIOS[0].id); throw new Error(SECRET); },
    cleanup(report) {
      return cleanupResources([
        { name: 'browser', close() { attempted.push('browser'); throw new Error(SECRET); } },
        { name: 'real_fixture', close() { attempted.push('real_fixture'); return new Promise(() => {}); } },
        { name: 'injected_fixture', close() { attempted.push('injected_fixture'); } },
      ], report, 10);
    },
  }));
  assert.equal(passed, false);
  assert.deepEqual(attempted, ['browser', 'real_fixture', 'injected_fixture']);
  const data = JSON.parse((await artifacts(output))[0]);
  assert.equal(data.failure.category, 'assertion_or_runtime_failure');
  assert.deepEqual(data.cleanup.map(item => item.status), ['failed', 'failed', 'passed']);
});

test('cleanup failure makes an otherwise fully passed journey fail without inventing a failed group', async t => {
  const output = join(await workspace(t), 'browser');
  assert.equal(await runHarness(defaults(output, {
    journey: complete,
    cleanup: report => cleanupResources([{ name: 'browser', close() { throw new Error(SECRET); } }], report),
  })), false);
  const [json, , xml] = await artifacts(output);
  const data = JSON.parse(json);
  assert.equal(data.failure, null);
  assert.deepEqual(data.counts, { passed: 18, failed: 0, not_run: 0 });
  assert.equal(data.status, 'failed');
  assert.match(xml, /browser: cleanup_failed/);
});

test('failure diagnostics occur before cleanup; diagnostic rejection never replaces primary failure', async t => {
  const output = join(await workspace(t), 'browser');
  const actions = [];
  assert.equal(await runHarness(defaults(output, {
    setup(report) { report.setStage('browser_launch'); throw new Error(SECRET); },
    diagnose() { actions.push('diagnose'); throw new Error(SECRET); },
    cleanup() { actions.push('cleanup'); },
  })), false);
  assert.deepEqual(actions, ['diagnose', 'cleanup']);
  const data = JSON.parse((await artifacts(output))[0]);
  assert.equal(data.failure.stage, 'browser_launch');
  assert.equal(data.failure_screenshot, 'unavailable');
  assert.deepEqual((await readdir(output)).sort(), ['junit.xml', 'result.json', 'summary.md']);
});

test('deadline stops a hanging journey, attempts cleanup and records static deadline failure', async t => {
  const output = join(await workspace(t), 'browser');
  let cleaned = false;
  assert.equal(await runHarness(defaults(output, {
    deadlineMs: 10,
    journey(report) { report.start(SCENARIOS[0].id); return new Promise(() => {}); },
    cleanup() { cleaned = true; },
  })), false);
  assert.equal(cleaned, true);
  const data = JSON.parse((await artifacts(output))[0]);
  assert.equal(data.failure.category, 'run_deadline_exceeded');
  assert.deepEqual(data.counts, { passed: 0, failed: 1, not_run: 17 });
});

test('SIGINT/SIGTERM use graceful cleanup and remove handlers', async t => {
  const root = await workspace(t);
  for (const signal of ['SIGINT', 'SIGTERM']) {
    const signalSource = new EventEmitter();
    let cleaned = false;
    const output = join(root, signal);
    assert.equal(await runHarness(defaults(output, {
      signalSource,
      journey() { queueMicrotask(() => signalSource.emit(signal)); return new Promise(() => {}); },
      cleanup() { cleaned = true; },
    })), false);
    assert.equal(cleaned, true);
    assert.equal(signalSource.listenerCount('SIGINT') + signalSource.listenerCount('SIGTERM'), 0);
    assert.equal(JSON.parse((await artifacts(output))[0]).failure.category, 'interrupted');
  }
});

test('only validated metadata and fixed-name SHA-256 fingerprints are retained', () => {
  const report = new Report({ FORGE_UI_QA_REVISION: 'b'.repeat(40), FORGE_UI_QA_SOURCE_SHA256: 'c'.repeat(64), FORGE_UI_QA_DIRTY: 'true' });
  report.addFingerprints({ 'app.wasm': 'd'.repeat(64), 'main.js': SECRET, [SECRET]: 'e'.repeat(64) });
  report.browserVersion('150.0.1.2');
  assert.equal(report.data.metadata.revision, 'b'.repeat(40));
  assert.equal(report.data.metadata.source_sha256, 'c'.repeat(64));
  assert.equal(report.data.metadata.dirty, 'true');
  assert.equal(report.data.metadata.browser_version, '150.0.1.2');
  assert.deepEqual(report.data.metadata.fingerprints_sha256, { 'app.wasm': 'd'.repeat(64) });
  assert.equal(JSON.stringify(report.data).includes(SECRET), false);
});

test('returning without all groups is a failure, not partial success', async t => {
  const output = join(await workspace(t), 'browser');
  assert.equal(await runHarness(defaults(output)), false);
  const data = JSON.parse((await artifacts(output))[0]);
  assert.equal(data.failure.category, 'incomplete_suite');
  assert.deepEqual(data.counts, { passed: 0, failed: 0, not_run: 18 });
});


test('late asynchronous acquisition after deadline is disposed before cleanup can pass', async t => {
  const output = join(await workspace(t), 'browser');
  let owner;
  let closed = false;
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  assert.equal(await runHarness(defaults(output, {
    deadlineMs: 10,
    setup: async report => {
      owner = acquireOwned(() => pending, () => { closed = true; });
      await owner.ready;
      report.ensureActive();
    },
    cleanup: report => {
      setTimeout(() => release({}), 10);
      return cleanupResources([{ name: 'browser', close: () => owner.close() }], report, 100);
    },
  })), false);
  assert.equal(closed, true);
  const data = JSON.parse((await artifacts(output))[0]);
  assert.equal(data.failure.category, 'run_deadline_exceeded');
  assert.equal(data.cleanup[0].status, 'passed');
});

test('timed-out acquisition cleanup is failed, with disposal still attached to a late resource', async t => {
  const output = join(await workspace(t), 'browser');
  let release;
  let closed = false;
  let owner;
  const pending = new Promise(resolve => { release = resolve; });
  assert.equal(await runHarness(defaults(output, {
    deadlineMs: 10,
    setup: async () => { owner = acquireOwned(() => pending, () => { closed = true; }); await owner.ready; },
    cleanup: report => cleanupResources([{ name: 'browser', close: () => owner.close() }], report, 10),
  })), false);
  assert.equal(JSON.parse((await artifacts(output))[0]).cleanup[0].status, 'failed');
  assert.equal(closed, false);
  release({});
  await owner.close();
  assert.equal(closed, true);
});


function screenshotFixture({ url = 'http://127.0.0.1:1234/', text = 'Controlled fixture workspace', markers = 1 } = {}) {
  const captures = [];
  const page = {
    isClosed: () => false, url: () => url,
    locator: selector => ({ selector, count: async () => markers }),
    // This test double checks only the screenshot gate/mask contract, not pixels.
    evaluate: async (_, pattern) => new RegExp(pattern, 'i').test(text),
    screenshot: async config => { captures.push(config); },
  };
  return { page, captures, fixtures: [{ page, origin: 'http://127.0.0.1:1234' }] };
}

test('failure screenshot is optional, fixture-owned and masks all pairing/editable controls', async () => {
  const { captures, fixtures } = screenshotFixture();
  const report = new Report();
  await captureFailureScreenshot(report, '/unused-test-path', fixtures, false);
  assert.equal(captures.length, 0);
  assert.equal(report.data.failure_screenshot, 'disabled');
  await captureFailureScreenshot(report, '/unused-test-path', fixtures, true);
  assert.equal(captures.length, 1);
  assert.equal(captures[0].mask[0].selector, SCREENSHOT_MASK);
  assert.equal(captures[0].timeout, 3000);
  assert.equal(captures[0].fullPage, false);
  assert.equal(report.data.failure_screenshot, 'captured');
});

test('failure screenshots skip unowned destinations, extra URL state and unknown pages', async () => {
  for (const options of [{ url: 'https://example.com/' }, { url: 'http://127.0.0.1:1234/?secret=value' }, { markers: 0 }]) {
    const { captures, fixtures } = screenshotFixture(options);
    const report = new Report();
    await captureFailureScreenshot(report, '/unused-test-path', fixtures, true);
    assert.equal(captures.length, 0);
    assert.equal(report.data.failure_screenshot, 'skipped_unsafe_page');
  }
});

test('failure screenshots skip rendered credential patterns without retaining the text', async () => {
  for (const text of [SECRET, 'Authorization: credential', 'cookie=credential', 'password: credential', 'f'.repeat(64)]) {
    const { captures, fixtures } = screenshotFixture({ text });
    const report = new Report();
    await captureFailureScreenshot(report, '/unused-test-path', fixtures, true);
    assert.equal(captures.length, 0);
    assert.equal(report.data.failure_screenshot, 'skipped_credential_pattern');
    assert.equal(JSON.stringify(report.data).includes(text), false);
  }
});

test('cleanup callback rejection after three successful records cannot be discarded as overflow', async t => {
  const output = join(await workspace(t), 'browser');
  assert.equal(await runHarness(defaults(output, {
    journey: complete,
    cleanup(report) {
      for (const name of ['browser', 'real_fixture', 'injected_fixture']) report.cleanup(name, 'passed', 0);
      throw new Error(SECRET);
    },
  })), false);
  const data = JSON.parse((await artifacts(output))[0]);
  assert.equal(data.status, 'failed');
  assert.equal(data.cleanup.find(item => item.name === 'browser').status, 'failed');
});


test('unexpected fixture leader exit records unconfirmed cleanup without signalling its old group', async () => {
  // A real, isolated short-lived child exercises the exit state. No descendant
  // discovery or actual browser/fixture E2E behavior is claimed by this test.
  const child = spawn(process.execPath, ['-e', 'process.exit(0)'], { stdio: 'ignore' });
  await once(child, 'exit');
  let signalAttempted = false;
  const report = new Report();
  await cleanupResources([{ name: 'real_fixture', close() {
    requireRunningFixtureLeader(child);
    signalAttempted = true;
  } }], report);
  assert.equal(signalAttempted, false);
  assert.equal(report.data.cleanup[0].status, 'failed');
  assert.equal(report.data.cleanup[0].category, 'cleanup_failed');
  assert.doesNotThrow(() => requireRunningFixtureLeader({ pid: 123, exitCode: null, signalCode: null }));
  assert.doesNotThrow(() => requireRunningFixtureLeader({ pid: undefined, exitCode: -2, signalCode: null }));
  assert.throws(() => requireRunningFixtureLeader({ pid: 123, exitCode: null, signalCode: 'SIGTERM' }));
});
