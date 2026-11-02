// Test-only browser driver. Not generated application JavaScript or a runtime asset.
// Every happy-path request uses the real generated worker/WASM -> host -> mTLS agent.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, realpath, writeFile } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { chromium, expect } from '@playwright/test';

const args = process.argv.slice(2);
if (args.length !== 4) {
  throw new Error('usage: node journey.mjs ASSETS_DIRECTORY AGENT_BINARY FIXTURE_BINARY OUTPUT_DIRECTORY');
}
const [assets, agent, fixtureBinary] = await Promise.all(args.slice(0, 3).map(p => realpath(p)));
const output = args[3];
await mkdir(output, { recursive: true });
const check = expect.configure({ timeout: 10_000 });
const checks = [];
let browser;
const active = new Set();
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function startFixture(faults = false) {
  const child = spawn(fixtureBinary, [assets, agent, ...(faults ? ['--faults'] : [])], {
    stdio: ['ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32',
  });
  const state = { child, stopped: false, origin: '', code: '' };
  active.add(state);
  let startupBytes = 0;
  // Do not retain/log stdout containing the pairing code, stderr, credentials or headers.
  child.stderr.on('data', bytes => { startupBytes += bytes.length; });
  const lines = createInterface({ input: child.stdout });
  let timer;
  try {
    await Promise.race([
      new Promise((resolve, reject) => {
        child.once('error', reject);
        child.once('exit', () => reject(new Error('fixture exited before readiness')));
        lines.on('line', line => {
          startupBytes += Buffer.byteLength(line);
          if (startupBytes > 65_536) return reject(new Error('fixture startup output limit'));
          if (/^READY http:\/\/127\.0\.0\.1:\d+$/.test(line)) state.origin = line.slice(6);
          if (/^PAIRING [0-9a-f]{64}$/.test(line)) state.code = line.slice(8);
          if (state.origin && state.code) resolve();
        });
      }),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('fixture startup deadline')), 15_000); }),
    ]);
    return state;
  } catch (error) {
    await stopFixture(state);
    throw error;
  } finally {
    clearTimeout(timer);
    lines.close();
  }
}
async function stopFixture(state) {
  if (state.stopped) return;
  state.stopped = true;
  state.code = '';
  if (state.child.exitCode === null && state.child.signalCode === null) {
    const exited = once(state.child, 'exit');
    const signal = name => {
      try {
        if (process.platform === 'win32') state.child.kill(name);
        else process.kill(-state.child.pid, name); // only our newly created process group
      } catch (error) { if (error.code !== 'ESRCH') throw error; }
    };
    signal('SIGINT');
    let timer;
    const clean = await Promise.race([exited.then(() => true), new Promise(resolve => { timer = setTimeout(() => resolve(false), 10_000); })]);
    clearTimeout(timer);
    if (!clean) { signal('SIGKILL'); await exited; }
  }
  active.delete(state);
}
async function connect(context, fixture) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', () => { if (errors.length < 64) errors.push('page runtime error'); });
  page.on('dialog', async dialog => { errors.push('unexpected script dialog'); await dialog.dismiss(); });
  await page.goto(fixture.origin, { waitUntil: 'domcontentloaded' });
  await check(page.locator('#pair')).toBeEnabled();
  await page.locator('#pair-code').fill(fixture.code);
  await page.locator('#pair').click();
  fixture.code = '';
  await check(page.locator('#status')).toContainText('Connected.');
  await check(page.locator('#pair-code')).toHaveValue('');
  await check(page.locator('#pair-panel')).toBeHidden();
  await check(page.locator('#node')).toHaveValue('lab');
  assert.equal(page.workers().length, 1, 'one real dedicated WASM worker');
  return { page, errors };
}
async function selectFile(page, name) {
  const item = page.locator('#files [role="treeitem"]').filter({ has: page.getByText(name, { exact: true }) });
  await item.click();
  return item;
}
async function realJourney() {
  const fixture = await startFixture();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  try {
    const { page, errors } = await connect(context, fixture);
    await page.locator('#project').selectOption('workspace');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('Partial inspection:');
    await check(page.locator('#summary')).toContainText('1 traits');
    await check(page.locator('#summary')).toContainText('1 trait impls');
    await check(page.locator('#summary')).toContainText('1 free functions');
    await check(page.locator('#files')).toContainText('README <safe>.md');
    await check(page.locator('#files')).toContainText('<svg onload=alert(1)>.txt');
    await check(page.locator('#files svg, #files script, #files img')).toHaveCount(0);
    if (process.platform !== 'win32') await check(page.locator('#files')).toContainText('unix bytes:');
    await page.getByRole('button', { name: 'Expand src', exact: true }).click();
    const file = await selectFile(page, 'lib.rs');
    await check(page.locator('#outline')).toContainText('Widget');
    await check(page.locator('#outline')).toContainText('View');
    await check(page.locator('#outline')).toContainText('inspect_me');
    await check(page.locator('#details')).toContainText('Lines');
    await page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ }).click();
    await check(page.locator('#details')).toContainText('Span');
    await check(page.locator('#details')).toContainText('end exclusive');
    checks.push('real generated UI / WASM / host / mTLS agent inspection and declaration details');

    await selectFile(page, 'broken.rs');
    await check(page.locator('#details')).toContainText('Diagnostic');
    // Keyboard navigation and repeated delegated handlers retain a single DOM row set.
    await file.focus();
    await page.keyboard.press('ArrowUp');
    await check(page.locator('#files [role="treeitem"]:focus')).toHaveCount(1);
    await file.click();
    const before = await page.locator('#files [role="treeitem"]').count();
    for (let i = 0; i < 3; i += 1) {
      await file.click();
      await check(file).toBeFocused();
      await check(page.locator('#files [role="treeitem"]')).toHaveCount(before);
    }
    assert.deepEqual(errors, []);
    await page.screenshot({ path: join(output, 'ide-inspection.png'), fullPage: true, mask: [page.locator('#pair-code')] });
    checks.push('partial diagnostic, escaped names, keyboard focus and repeated event lifecycle');

    await page.locator('#project').selectOption('denied');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('operation forbidden');
    await check(page.locator('#inspect')).toBeEnabled();
    checks.push('independent agent project authorization denial through real mTLS');

    await page.reload({ waitUntil: 'domcontentloaded' });
    await check(page.locator('#status')).toContainText('Connected.');
    await check(page.locator('#pair-panel')).toBeHidden();
    assert.equal(page.workers().length, 1, 'refresh owns one worker');
    await page.locator('#project').selectOption('workspace');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('Partial inspection:');
    assert.deepEqual(errors, []);
    checks.push('refresh resumes admitted cookie session without pairing-code persistence');
  } finally { await context.close(); await stopFixture(fixture); }
}
async function controlledLifecycleJourney() {
  // Explicit native injected-port fixture, NOT claimed as remote transport E2E.
  // It preserves the real browser, generated worker/WASM and host admission path.
  const fixture = await startFixture(true);
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  try {
    const { page, errors } = await connect(context, fixture);
    await page.locator('#project').selectOption('slow');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('Inspecting');
    await page.locator('#cancel').click();
    await check(page.locator('#status')).toContainText('Stopped waiting locally.');
    // The trusted fake completes after cancellation; its late result must not revive UI state.
    await delay(1000);
    await check(page.locator('#status')).toContainText('Stopped waiting locally.');
    await check(page.locator('#files')).not.toContainText('fault-fixture.txt');
    await page.locator('#project').selectOption('workspace');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('Inspection ready.');
    await check(page.locator('#files')).toContainText('fault-fixture.txt');
    checks.push('controlled injected-port local cancellation, late-result rejection and explicit retry');

    await page.locator('#project').selectOption('slow');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('Inspecting');
    await page.locator('#project').selectOption('workspace');
    await delay(1000);
    await check(page.locator('#status')).toContainText('Selection changed.');
    await check(page.locator('#files')).not.toContainText('fault-fixture.txt');
    checks.push('controlled selection revision invalidates stale in-flight work');

    await page.locator('#project').selectOption('timeout');
    await page.locator('#inspect').click();
    await check(page.locator('#status')).toContainText('Deadline expired');
    await check(page.locator('#inspect')).toBeEnabled();
    await delay(700); // allow the non-preemptible fake's final completion
    await check(page.locator('#status')).toContainText('Deadline expired');
    assert.deepEqual(errors, []);
    await page.screenshot({ path: join(output, 'ide-controlled-timeout.png'), fullPage: true, mask: [page.locator('#pair-code')] });
    checks.push('controlled non-preemptible timeout remains failure, never late success');
  } finally { await context.close(); await stopFixture(fixture); }
}
try {
  // Sandbox is deliberately enabled. No ignore-certificate/CSP/security overrides,
  // route interception, fake WASM, external deployment or user browser profile.
  const channel = process.env.FORGE_UI_BROWSER_CHANNEL;
  if (channel !== undefined && channel !== 'chrome') throw new Error('unsupported browser channel');
  browser = await chromium.launch({ headless: true, chromiumSandbox: true, ...(channel ? { channel } : {}) });
  await realJourney();
  await controlledLifecycleJourney();
  await writeFile(join(output, 'result.json'), `${JSON.stringify({ status: 'passed', checks }, null, 2)}\n`);
  console.log(`PASS: ${checks.length} real-browser checks (real-agent and controlled-fault scopes distinguished).`);
} catch (error) {
  await writeFile(join(output, 'result.json'), `${JSON.stringify({ status: 'failed', completed: checks }, null, 2)}\n`);
  console.error(String(error.message).replace(/\b[0-9a-f]{64}\b/gi, "<redacted-token>"));
  process.exitCode = 1;
} finally {
  if (browser) await browser.close();
  for (const state of active) await stopFixture(state);
}
