// Test-only browser driver. Not generated application JavaScript or a runtime asset.
// Every happy-path request uses the real generated worker/WASM -> host -> mTLS agent.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { acquireOwned, bounded, cleanupResources, runHarness } from './report.mjs';
import { captureFailureScreenshot, screenshotEligibility, SCREENSHOT_MASK } from './diagnostics.mjs';
import { VIEWPORTS, verifyViewportProfiles } from './viewports.mjs';
import { requireRunningFixtureLeader } from './fixture-lifecycle.mjs';

let assets, agent, fixtureBinary, output, browser, browserOwner, check, report;
const active = new Set();
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function startFixture(faults = false) {
  report.ensureActive();
  report.setStage('fixture_start');
  const child = spawn(fixtureBinary, [assets, agent, ...(faults ? ['--faults'] : [])], {
    stdio: ['ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32',
  });
  const state = { child, stopped: false, stopping: null, origin: '', code: '', page: null, name: faults ? 'injected_fixture' : 'real_fixture' };
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
  } finally {
    clearTimeout(timer);
    lines.close();
  }
}
async function stopFixture(state) {
  if (state.stopped) return;
  if (state.stopping) return state.stopping;
  state.code = '';
  state.stopping = (async () => {
    const child = state.child;
    requireRunningFixtureLeader(child);
    if (child.pid && child.exitCode === null && child.signalCode === null) {
      const signal = name => {
        try {
          if (process.platform === 'win32') child.kill(name);
          else process.kill(-child.pid, name); // only our newly created process group
        } catch (error) { if (error.code !== 'ESRCH') throw error; }
      };
      // Register before signalling, and bound both graceful and forceful shutdown.
      let resolveExit;
      const exited = new Promise(resolve => { resolveExit = resolve; });
      const onExit = () => resolveExit();
      child.once('exit', onExit);
      try {
        signal('SIGINT');
        try { await bounded(() => exited, 10_000); }
        catch { signal('SIGKILL'); await bounded(() => exited, 3_000); }
      } finally { child.off('exit', onExit); }
    }
    state.stopped = true;
    active.delete(state);
  })();
  try { await state.stopping; }
  finally { state.stopping = null; }
}
const LONG_FILE_NAME = 'remote_project_request_validation_and_admission_contract_with_a_deliberately_long_safe_name_for_filtering.rs';
const localActions = ['select_entry', 'toggle_entry', 'select_declaration', 'select_problem', 'set_file_filter', 'set_symbol_filter', 'more_tree', 'previous_tree', 'more_outline', 'previous_outline', 'more_problems', 'previous_problems'];

// Test-only observation: never replace the Worker implementation, WASM or gateway.
// Retain only local report actions in ephemeral page memory, never pairing events,
// request headers, cookies or network bodies. Nothing in this probe is an artifact.
async function observe(context) {
  const counts = new Map();
  context.on('request', request => {
    if (new URL(request.url()).pathname !== '/forge/v1/dispatch') return;
    let kind;
    try { kind = request.postDataJSON()?.kind; } catch { kind = 'invalid'; }
    if (!['resume', 'pair', 'inspect'].includes(kind)) kind = 'other';
    counts.set(kind, (counts.get(kind) || 0) + 1);
  });
  await context.addInitScript(kinds => {
    const allowed = new Set(kinds);
    const original = Worker.prototype.postMessage;
    const probe = { worker: null, actions: new Map(), saved: new Map(), counts: new Map(), seenWorkers: new WeakSet(), views: 0 };
    Worker.prototype.postMessage = function(message, ...rest) {
      probe.worker = this;
      if (!probe.seenWorkers.has(this)) {
        probe.seenWorkers.add(this);
        this.addEventListener('message', event => { if (event.data?.kind === 'view') probe.views += 1; });
      }
      const event = message?.event ?? message;
      if (allowed.has(event?.kind)) { probe.actions.set(event.kind, structuredClone(event)); probe.counts.set(event.kind, (probe.counts.get(event.kind) || 0) + 1); }
      return original.call(this, message, ...rest);
    };
    window.__forgeBrowserProbe = {
      count(kind) { return probe.counts.get(kind) || 0; },
      receivedViews() { return probe.views; },
      save(kind, slot = kind) {
        if (!probe.actions.has(kind)) throw new Error('Expected local event was not emitted');
        const action = probe.actions.get(kind);
        if (!action.report || typeof action.report.session_epoch !== 'string' ||
            typeof action.report.request_id !== 'string' || !Number.isSafeInteger(action.report.revision)) {
          throw new Error('Report-scoped event omitted its accepted report context');
        }
        if (kind === 'set_symbol_filter' && !Number.isSafeInteger(action.entry)) {
          throw new Error('Symbol filter omitted its selected-entry context');
        }
        if (kind.endsWith('_filter') && (typeof action.value !== 'string' || new TextEncoder().encode(action.value).length > 256)) {
          throw new Error('Filter event omitted its bounded query value');
        }
        probe.saved.set(slot, structuredClone(action));
      },
      replay(slot) {
        if (!probe.saved.has(slot) || !probe.worker) throw new Error('Missing stale-event probe');
        return new Promise((resolve, reject) => {
          const timer = setTimeout(() => { probe.worker.removeEventListener('message', receive); reject(new Error('Stale intent did not receive a bounded view response')); }, 10_000);
          const receive = event => {
            if (event.data?.kind !== 'view') return;
            probe.worker.removeEventListener('message', receive);
            clearTimeout(timer);
            resolve();
          };
          probe.worker.addEventListener('message', receive);
          original.call(probe.worker, structuredClone(probe.saved.get(slot)));
        });
      },
      clear() { probe.actions.clear(); probe.saved.clear(); probe.worker = null; },
    };
  }, localActions);
  return { count: kind => counts.get(kind) || 0 };
}
async function connect(context, fixture) {
  report.ensureActive();
  const page = await context.newPage();
  fixture.page = page;
  const errors = [];
  page.on('pageerror', () => { if (errors.length < 64) errors.push('page runtime error'); });
  page.on('dialog', dialog => {
    if (errors.length < 64) errors.push('unexpected script dialog');
    void dialog.dismiss().catch(() => { if (errors.length < 64) errors.push('dialog dismissal failed'); });
  });
  await page.goto(fixture.origin, { waitUntil: 'domcontentloaded' });
  await check(page.locator('#pair')).toBeEnabled();
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#project option')).toHaveCount(0);
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
function fileRow(page, name) {
  return page.locator('#files [role="treeitem"]').filter({ has: page.getByText(name, { exact: true }) });
}
async function selectFile(page, name) {
  const item = fileRow(page, name);
  await item.scrollIntoViewIfNeeded();
  const before = await item.boundingBox();
  await page.mouse.move(before.x + before.width / 2, before.y + before.height / 2);
  await page.mouse.down();
  const pressed = await item.boundingBox();
  const pressStable = Math.abs(before.y - pressed.y) < 1 && Math.abs(before.height - pressed.height) < 1;
  if (!pressStable) process.stderr.write('QA_CHECK pointer_pressed_stability_failed\n');
  assert.ok(pressStable, 'focus readout never moves the pressed pointer target');
  await page.mouse.up();
  await check(item).toHaveAttribute('aria-selected', 'true');
  const after = await item.boundingBox();
  const tree = await page.locator('#files').boundingBox();
  // Accepted selection may remove a "selection hidden" summary and move rows.
  // It must not scroll the focused selected row out of its owned viewport.
  const projectionVisible = after.y >= tree.y - 1 && after.y + after.height <= tree.y + tree.height + 1;
  if (!projectionVisible) process.stderr.write('QA_CHECK pointer_projection_visibility_failed\n');
  assert.ok(projectionVisible, 'selected row stays inside its scrolling tree after projection');
  await check(page.locator('#selection-title')).toContainText(name);
  return item;
}
async function filterFiles(page, query) {
  await page.locator('#file-filter').fill(query);
  // An accepted view updates the summary as well as the input, so the caller's
  // eventual row assertions, not fill() alone, prove WASM handled the query.
}
async function noReport(page) {
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#outline [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#declarations [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#problems [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#source-shape [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#source-shape-details')).toHaveText('');
  await check(page.locator('#details')).not.toContainText('fault-fixture.txt');
  await check(page.locator('#details')).not.toContainText(LONG_FILE_NAME);
  await check(page.locator('#declaration-details')).not.toContainText('inspect_me');
}
async function screenshot(page, name) {
  assert.equal(await screenshotEligibility(page, [...active]), 'safe', 'Only sanitized owned fixture pages may be captured');
  await check(page.locator('#pair-code')).toHaveValue('');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, 'no page-level horizontal clipping');
  await page.screenshot({ path: join(output, name), fullPage: true, mask: [page.locator(SCREENSHOT_MASK)], maskColor: '#1b2a3b' });
}
async function assertVisibleFocus(page) {
  assert.equal(await page.evaluate(() => {
    const e = document.activeElement;
    return !!e && e !== document.body && !e.closest('[hidden], [inert]') && e.getClientRects().length > 0;
  }), true, 'focus belongs to a visible active control');
}
async function assertTone(page, id, tone) {
  await check(page.locator(`#${id}`)).toHaveAttribute('data-tone', tone);
  assert.equal(await page.locator(`#${id}`).evaluate((node, token) => {
    const reference = document.createElement('span');
    reference.style.color = `var(--${token})`;
    document.body.append(reference);
    const matches = getComputedStyle(node).color === getComputedStyle(reference).color;
    reference.remove();
    return matches;
  }, tone), true, 'typed status tone survives the stylesheet cascade');
}
async function inspect(page, project, expected = 'Partial inspection:') {
  await page.locator('#project').selectOption(project);
  await page.locator('#inspect').click();
  await check(page.locator('#status')).toContainText(expected);
}
async function saveAction(page, kind, slot = kind) {
  await page.evaluate(([k, s]) => window.__forgeBrowserProbe.save(k, s), [kind, slot]);
}
async function replayAction(page, slot) {
  await page.evaluate(s => window.__forgeBrowserProbe.replay(s), slot);
}

async function keyboardAndFilters(page, traffic) {
  report.start('real.navigation.keyboard');
  const calls = traffic.count('inspect');
  await page.getByRole('button', { name: 'Expand src', exact: true }).click();
  const lib = await selectFile(page, 'lib.rs');
  await check(page.locator('#outline')).toContainText('Widget');
  await check(page.locator('#selection-title')).toHaveJSProperty('tagName', 'H3');
  await check(page.locator('#outline')).toContainText('View');
  await check(page.locator('#outline')).toContainText('inspect_me');
  await check(page.locator('#details')).toContainText('Lines');
  await page.locator('#declarations').getByRole('treeitem', { name: /^inspect_me\b/ }).click();
  await check(page.locator('#declaration-details')).toContainText('inspect_me');
  await check(page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ })).toHaveAttribute('aria-selected', 'true');
  const selected = await lib.getAttribute('aria-selected');
  await lib.focus();
  await page.keyboard.press('ArrowUp');
  await check(lib).not.toBeFocused();
  await check(lib).toHaveAttribute('aria-selected', selected);
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  const src = fileRow(page, 'src');
  await src.focus();
  await page.keyboard.press('ArrowRight');
  await check(lib).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await check(src).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await check(src).toHaveAttribute('aria-expanded', 'false');
  await page.keyboard.press('ArrowRight');
  await check(src).toHaveAttribute('aria-expanded', 'true');
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  await page.keyboard.press('Home');
  await check(page.locator('#files [role="treeitem"]').first()).toBeFocused();
  await page.keyboard.press('End');
  await check(page.locator('#files [role="treeitem"]').last()).toBeFocused();
  await page.keyboard.press('Home');
  await page.keyboard.type('broken');
  await check(fileRow(page, 'broken.rs')).toBeFocused();
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  await page.keyboard.press('Enter');
  await check(fileRow(page, 'broken.rs')).toHaveAttribute('aria-selected', 'true');
  await check(page.locator('#details')).toContainText(/diagnostic/i);
  await check(page.locator('#details')).toContainText('Rust syntax analysis unavailable');
  report.pass('real.navigation.keyboard');
  report.start('real.filters.safe_text');

  await selectFile(page, 'lib.rs');
  await page.locator('#symbol-filter').fill('inspect_me');
  await check(page.locator('#outline [role="treeitem"]')).toHaveCount(1);
  const symbol = page.locator('#outline [role="treeitem"]');
  await symbol.focus();
  await page.keyboard.press('Enter');
  await check(page.locator('#declaration-details')).toContainText('Span');
  await check(page.locator('#declaration-details')).toContainText('end exclusive');
  await saveAction(page, 'set_symbol_filter', 'old-symbol-filter');
  await page.locator('#symbol-filter').fill('NO-SUCH-SYMBOL');
  await check(page.locator('#outline [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#declaration-details')).toContainText('inspect_me');
  await check(page.locator('#symbol-filter-summary')).toContainText(/hidden by filter/i);
  await page.locator('#symbol-filter').fill('');
  await check(page.locator('#outline')).toContainText('Widget');

  await filterFiles(page, '');
  await page.locator('#file-filter').pressSequentially('zz-pagination-matchX');
  await page.locator('#file-filter').press('Backspace');
  await check(page.locator('#file-filter')).toHaveValue('zz-pagination-match');
  await check(page.locator('#files')).toContainText('bulk');
  await check(page.locator('#files')).toContainText('zz-pagination-match.txt');
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  await check(page.locator('#file-filter-summary')).toContainText(/hidden by filter/i);
  await filterFiles(page, 'deliberately_long_safe_name');
  await selectFile(page, LONG_FILE_NAME);
  await check(page.locator('#details')).toContainText(LONG_FILE_NAME);
  await filterFiles(page, '<svg onload=alert(1)>');
  await selectFile(page, '<svg onload=alert(1)>.txt');
  await check(page.locator('#details')).toContainText('<svg onload=alert(1)>.txt');
  await check(page.locator('#files svg, #files script, #files img, #details svg, #details script, #details img')).toHaveCount(0);
  if (process.platform !== 'win32') {
    await filterFiles(page, 'unix bytes:');
    const native = page.locator('#files [role="treeitem"]').filter({ hasText: 'unix bytes:' });
    await check(native).toHaveCount(1);
    const safeName = await native.locator('.fg-row-label').textContent();
    await native.click();
    await check(page.locator('#details')).toContainText(safeName);
    assert.match(safeName, /ff/i, 'native representation preserves the non-UTF8 byte');
  }
  const filterEvents = await page.evaluate(() => window.__forgeBrowserProbe.count('set_file_filter'));
  await page.locator('#file-filter').fill('é'.repeat(129));
  assert.equal(await page.locator('#file-filter').evaluate(input => input.validity.valid), false, 'UTF-8 byte bound is enforced, not merely character count');
  assert.equal(await page.evaluate(() => window.__forgeBrowserProbe.count('set_file_filter')), filterEvents, 'oversized filter never reaches the worker');
  await filterFiles(page, 'NO-MATCH');
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#files')).toContainText(/match/i);
  await filterFiles(page, '');
  await check(fileRow(page, 'blank.rs')).toBeVisible();
  await selectFile(page, 'blank.rs');
  await check(page.locator('#outline')).toContainText(/No declarations were emitted/i);
  await replayAction(page, 'old-symbol-filter');
  await check(page.locator('#symbol-filter')).toHaveValue('');
  await check(page.locator('#selection-title')).toContainText('blank.rs');
  await selectFile(page, 'binary.dat');
  await check(page.locator('#details')).toContainText(/binary/i);
  await check(page.locator('#outline')).not.toContainText(/no declarations in this file/i);
  report.pass('real.filters.safe_text');
  report.start('real.pagination.problems');

  await page.getByRole('button', { name: 'Expand bulk', exact: true }).click();
  await check(page.locator('#tree-more')).toBeEnabled();
  const pagingBefore = await page.locator('#tree-more').boundingBox();
  await page.locator('#files [role="treeitem"]').first().focus();
  await page.keyboard.press('End');
  await check(page.locator('#files [role="treeitem"]').last()).toBeFocused();
  assert.ok(await page.locator('#files').evaluate(tree => tree.scrollTop > 0), 'large trees own their scroll');
  const pagingAfter = await page.locator('#tree-more').boundingBox();
  assert.ok(Math.abs(pagingBefore.y - pagingAfter.y) < 1, 'paging stays visible when the tree scrolls');
  assert.ok(pagingAfter.y + pagingAfter.height <= (await page.locator('#explorer-panel').boundingBox()).y + (await page.locator('#explorer-panel').boundingBox()).height, 'paging stays inside its pane');
  const firstPage = await page.locator('#files [role="treeitem"]').evaluateAll(rows => rows.map(row => row.parentElement.dataset.rowId));
  assert.ok(firstPage.length > 0 && firstPage.length <= 256, 'row count and byte budget bound the first window');
  await check(page.locator('#files')).not.toContainText('zz-pagination-match.txt');
  await page.locator('#tree-more').click();
  await check(page.locator('#files')).toContainText('zz-pagination-match.txt');
  const secondPage = await page.locator('#files [role="treeitem"]').evaluateAll(rows => rows.map(row => row.parentElement.dataset.rowId));
  assert.ok(secondPage.length > 0 && secondPage.length <= 256, 'each page remains bounded');
  assert.ok(secondPage.every(id => !firstPage.includes(id)), 'Next replaces the visible window rather than duplicating rows');
  await page.locator('#tree-previous').click();
  await check.poll(() => page.locator('#files [role="treeitem"]').evaluateAll(rows => rows.map(row => row.parentElement.dataset.rowId))).toEqual(firstPage);
  // Filtering searches beyond the current page and keeps the ancestor context.
  await filterFiles(page, 'zz-pagination-match');
  await check(page.locator('#files')).toContainText('bulk');
  await check(page.locator('#files')).toContainText('zz-pagination-match.txt');
  await filterFiles(page, '');
  await page.getByRole('button', { name: 'Collapse bulk', exact: true }).click();
  await check(fileRow(page, 'broken.rs')).toBeVisible();
  const problem = page.locator('#problems [role="treeitem"]').filter({ hasText: 'broken.rs' }).first();
  await check(problem).toBeVisible();
  await problem.click();
  await check(page.locator('#selection-title')).toContainText('broken.rs');
  await check(page.locator('#details')).toContainText(/diagnostic/i);
  await check(page.locator('#declaration-details')).not.toContainText('inspect_me');
  assert.equal(traffic.count('inspect'), calls, 'all navigation/filter/page/problem actions are local, not backend search');
  report.pass('real.pagination.problems');
}

async function panesAndNarrow(page, traffic) {
  report.start('real.panes.desktop');
  const calls = traffic.count('inspect');
  await filterFiles(page, 'lib.rs');
  await selectFile(page, 'lib.rs');
  const center = await page.locator('#workspace-panel').boundingBox();
  const explorer = await page.locator('#explorer-panel').boundingBox();
  const outline = await page.locator('#outline-panel').boundingBox();
  assert.ok(center.width > explorer.width && center.width > outline.width, 'file workspace is the dominant desktop surface');
  await assertTone(page, 'status', 'warning');
  await check(page.locator('#cancel')).toBeHidden();
  await assertTone(page, 'phase-label', 'warning');
  await page.locator('#project').focus();
  await page.keyboard.press('Tab');
  await check(page.locator('#inspect')).toBeFocused();
  assert.equal(await page.locator('#inspect').evaluate(button => {
    const style = getComputedStyle(button);
    return button.matches(':focus-visible') && style.outlineStyle === 'solid' &&
      Number.parseFloat(style.outlineOffset) > 0 && style.outlineColor !== style.backgroundColor;
  }), true, 'primary action has a contrasting external keyboard focus ring');
  await screenshot(page, 'ide-workbench-desktop.png');
  for (const pane of ['explorer-panel', 'outline-panel', 'problems-panel']) {
    // aria-controls makes the split-pane relation part of the observable contract.
    const splitter = page.locator(`[role="separator"][aria-controls="${pane}"]`);
    await check(splitter).toHaveCount(1);
    const initial = Number(await splitter.getAttribute('aria-valuenow'));
    const orientation = await splitter.getAttribute('aria-orientation');
    const increase = orientation === 'horizontal' ? 'ArrowUp' : pane === 'outline-panel' ? 'ArrowLeft' : 'ArrowRight';
    await splitter.focus();
    await page.keyboard.press(increase);
    await check(splitter).toHaveAttribute('aria-valuenow', String(initial + 10));
    await page.keyboard.press(`Shift+${increase}`);
    await check(splitter).toHaveAttribute('aria-valuenow', String(initial + 50));
    await page.keyboard.press('Home');
    await check(splitter).toHaveAttribute('aria-valuenow', await splitter.getAttribute('aria-valuemin'));
    if (pane === 'problems-panel') {
      const pager = page.locator('#problems-previous');
      await pager.scrollIntoViewIfNeeded();
      const control = await pager.boundingBox();
      const bounds = await page.locator(`#${pane}`).boundingBox();
      assert.ok(control.y >= bounds.y && control.y + control.height <= bounds.y + bounds.height + 1, 'minimum-height pane keeps paging reachable through its scroll fallback');
      await splitter.focus();
    }
    await page.keyboard.press('End');
    await check(splitter).toHaveAttribute('aria-valuenow', await splitter.getAttribute('aria-valuemax'));
    if (!await page.locator('#workspace-options').evaluate(e => e.open)) await page.locator('#workspace-options > summary').click();
    await page.locator('#workspace-reset').click();
    await check(splitter).toHaveAttribute('aria-valuenow', String(initial));
    const bounds = await splitter.boundingBox();
    const x = bounds.x + bounds.width / 2;
    const y = bounds.y + bounds.height / 2;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + (pane === 'explorer-panel' ? 20 : pane === 'outline-panel' ? -20 : 0), y + (orientation === 'horizontal' ? -20 : 0));
    await page.mouse.up();
    await check(splitter).toHaveAttribute('aria-valuenow', String(initial + 20));
    await page.locator('#workspace-reset').click();
    await check(splitter).toHaveAttribute('aria-valuenow', String(initial));
    await splitter.focus();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeHidden();
    await check(page.locator(`#${pane}-toggle`)).toBeFocused();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeVisible();
    await assertVisibleFocus(page);
    await page.locator(`#${pane}-close`).focus();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}-toggle`)).toBeFocused();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeVisible();
  }
  if (await page.locator('#workspace-options').evaluate(e => e.open)) await page.locator('#workspace-options > summary').click();
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  report.pass('real.panes.desktop');
  report.start('real.panes.narrow');

  await page.setViewportSize({ width: 760, height: 1000 });
  await check(page.locator('#workspace-panel')).toBeVisible();
  await check(page.locator('#explorer-panel')).toBeHidden();
  await check(page.locator('#outline-panel')).toBeHidden();
  await check(page.locator('#problems-panel')).toBeHidden();
  await screenshot(page, 'ide-workbench-narrow-details.png');
  for (const pane of ['explorer-panel', 'outline-panel', 'problems-panel', 'workspace-panel']) {
    await page.locator(`#${pane}-switch`).focus();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeVisible();
    for (const other of ['explorer-panel', 'outline-panel', 'problems-panel', 'workspace-panel'].filter(id => id !== pane)) {
      await check(page.locator(`#${other}`)).toBeHidden();
    }
    await assertVisibleFocus(page);
    await page.keyboard.press('Tab');
    await assertVisibleFocus(page);
    await check(page.locator('#selection-title')).toContainText('lib.rs');
  }
  await page.locator('#outline-panel-switch').click();
  const symbol = page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ });
  await symbol.focus();
  await page.keyboard.press('Enter');
  await page.locator('#workspace-panel-switch').click();
  await check(page.locator('#declaration-details')).toContainText('inspect_me');
  await page.locator('#outline-panel-switch').click();
  await check(symbol).toBeFocused();
  await page.locator('#workspace-panel-switch').click();
  assert.ok(await page.locator('#declaration-details-panel').evaluate(panel => {
    const p = panel.getBoundingClientRect();
    const body = panel.closest('.fg-pane-body').getBoundingClientRect();
    return p.top >= body.top && p.bottom <= Math.min(body.bottom, innerHeight);
  }), 'selected declaration is readable without scrolling past file metadata');
  await screenshot(page, 'ide-workbench-narrow-declaration.png');
  await page.locator('#problems-panel-switch').click();
  await page.locator('#problems [role="treeitem"]').filter({ hasText: 'broken.rs' }).first().click();
  await page.locator('#workspace-panel-switch').click();
  await check(page.locator('#selection-title')).toContainText('broken.rs');
  await check(page.locator('#details')).toContainText(/diagnostic/i);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await check(page.locator('#workspace-panel')).toBeVisible();
  await check(page.locator('#explorer-panel')).toBeVisible();
  await check(page.locator('#outline-panel')).toBeVisible();
  await check(page.locator('#selection-title')).toContainText('broken.rs');
  assert.equal(page.workers().length, 1, 'pane changes never remount the worker');
  assert.equal(traffic.count('inspect'), calls, 'pane changes never inspect remotely');
  await screenshot(page, 'ide-workbench-partial-problems.png');
  report.pass('real.panes.narrow');
}

// Reuse the accepted real-agent fixture across fixed CSS viewport profiles. No
// new requests, device user agent, touch flags or WebKit claims are introduced.
async function deviceViewportJourney(page, traffic) {
  const calls = traffic.count('inspect');
  async function assertViewportFocus() {
    await assertVisibleFocus(page);
    await check.poll(() => page.evaluate(() => {
      const active = document.activeElement;
      const rect = active.getBoundingClientRect();
      const owner = active.closest('.fg-pane-body')?.getBoundingClientRect();
      return rect.left >= Math.max(0, owner?.left ?? 0) - 1 &&
        rect.right <= Math.min(innerWidth, owner?.right ?? innerWidth) + 1 &&
        rect.top >= Math.max(0, owner?.top ?? 0) - 1 &&
        rect.bottom <= Math.min(innerHeight, owner?.bottom ?? innerHeight) + 1;
    })).toBe(true);
  }
  for (const viewport of VIEWPORTS) {
    report.start(viewport.id);
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    assert.deepEqual(await page.evaluate(() => ({ width: innerWidth, height: innerHeight })), { width: viewport.width, height: viewport.height });
    const narrow = viewport.width <= 800;
    await check(page.locator('#workspace')).toHaveAttribute('data-narrow', String(narrow));
    async function activate(pane) {
      if (narrow) {
        const tab = page.locator(`#${pane}-switch`);
        await tab.focus();
        await page.keyboard.press('Enter');
        await check(tab).toHaveAttribute('aria-pressed', 'true');
        for (const other of ['explorer-panel', 'outline-panel', 'problems-panel', 'workspace-panel'].filter(id => id !== pane)) {
          await check(page.locator(`#${other}`)).toBeHidden();
        }
      } else {
        await page.locator(`#${pane}-heading`).focus();
      }
      await check(page.locator(`#${pane}`)).toBeVisible();
      await assertViewportFocus();
    }
    await activate('explorer-panel');
    await filterFiles(page, 'lib.rs');
    await selectFile(page, 'lib.rs');
    await activate('workspace-panel');
    await check(page.locator('#source-shape-limits')).toContainText('not a complexity score');
    const candidate = page.locator('#source-shape [data-row-id*="-candidate-"] [role="treeitem"]').filter({ hasText: 'Gadget' });
    await candidate.scrollIntoViewIfNeeded();
    await candidate.focus();
    await page.keyboard.press('Enter');
    await check(candidate).toBeFocused();
    await assertViewportFocus();
    await check(page.locator('#selection-title')).toContainText('lib.rs');
    await activate('outline-panel');
    const symbol = page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ });
    await symbol.focus();
    await page.keyboard.press('Enter');
    await assertViewportFocus();
    await activate('workspace-panel');
    await check(page.locator('#declaration-details')).toContainText('inspect_me');
    await check.poll(() => page.locator('#workspace-panel .fg-pane-body').evaluate(body => body.scrollTop)).toBe(0);
    // Low landscape heights legitimately scroll the page vertically. The selected
    // declaration remains reachable and readable without horizontal scrolling.
    const selectedName = page.locator('#declaration-details dd').first();
    await selectedName.scrollIntoViewIfNeeded();
    assert.equal(await selectedName.evaluate(node => {
      const rect = node.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0 && rect.left >= -1 && rect.right <= innerWidth + 1 && rect.top >= -1 && rect.bottom <= innerHeight + 1;
    }), true, 'selected declaration value is readable at this viewport');
    await activate('outline-panel');
    if (narrow) await check(symbol).toBeFocused();
    await symbol.focus();
    await page.keyboard.press('Tab');
    await assertViewportFocus();
    await activate('workspace-panel');
    assert.equal(await page.locator('#workspace-panel .fg-pane-body').evaluate(body => body.scrollWidth <= body.clientWidth + 1), true, 'main pane has no horizontal overflow');
    assert.equal(page.workers().length, 1, 'viewport changes keep one worker');
    assert.equal(traffic.count('inspect'), calls, 'viewport navigation remains report-local');
    await screenshot(page, viewport.screenshot);
    report.pass(viewport.id);
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  await check(page.locator('#workspace')).toHaveAttribute('data-narrow', 'false');
}

async function reportAndPageLifecycle(page, traffic) {
  report.start('probe.report.stale_actions');
  await filterFiles(page, LONG_FILE_NAME);
  await selectFile(page, LONG_FILE_NAME);
  await saveAction(page, 'select_entry', 'old-report-row');
  await saveAction(page, 'set_file_filter', 'old-report-filter');
  await inspect(page, 'workspace');
  await noSelectedFileFromPriorReport(page);
  await replayAction(page, 'old-report-row');
  await replayAction(page, 'old-report-filter');
  await check(page.locator('#file-filter')).toHaveValue('');
  await noSelectedFileFromPriorReport(page);
  const rows = await page.locator('#files [role="treeitem"]').count();
  const target = fileRow(page, 'README <safe>.md');
  for (let i = 0; i < 3; i += 1) {
    await target.click();
    await check(target).toBeFocused();
    await check(page.locator('#files [role="treeitem"]')).toHaveCount(rows);
  }
  report.pass('probe.report.stale_actions');
  report.start('probe.page.lifecycle');

  let before = traffic.count('resume');
  const inspectBefore = traffic.count('inspect');
  const cleared = await page.evaluate(() => {
    window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
    return {
      options: document.querySelectorAll('#node option, #project option').length,
      titlesCleared: [...document.querySelectorAll('select[data-action]')].every(select => !select.title),
      rows: document.querySelectorAll('#files [role="treeitem"], #outline [role="treeitem"], #declarations [role="treeitem"], #problems [role="treeitem"]').length,
      details: document.getElementById('details').textContent,
      declarations: document.getElementById('declaration-details').textContent,
      hidden: document.getElementById('workbench').hidden,
    };
  });
  assert.equal(cleared.options, 0, 'teardown synchronously clears protected catalog DOM');
  assert.equal(cleared.titlesCleared, true, 'teardown also removes catalog metadata from select titles');
  assert.equal(cleared.rows, 0, 'teardown synchronously clears report rows');
  assert.equal(cleared.details, '', 'teardown synchronously clears metadata');
  assert.equal(cleared.declarations, '', 'teardown synchronously clears declaration metadata');
  assert.equal(cleared.hidden, true, 'teardown hides protected composition before resume');
  await page.evaluate(() => {
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
  });
  await check(page.locator('#status')).toContainText('Connected.');
  await noReport(page);
  assert.equal(traffic.count('resume'), before + 1, 'repeated pageshow starts one fresh Resume');
  assert.equal(traffic.count('inspect'), inspectBefore, 'return never replays an inspection');
  assert.equal(page.workers().length, 1);
  before = traffic.count('resume');
  await page.reload({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#status')).toContainText('Connected.');
  await noReport(page);
  assert.equal(traffic.count('resume'), before + 1);
  assert.equal(traffic.count('inspect'), inspectBefore);
  assert.equal(page.workers().length, 1);
  await page.goto('about:blank');
  await page.goBack({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#status')).toContainText('Connected.');
  await noReport(page);
  assert.equal(traffic.count('inspect'), inspectBefore, 'browser leave/return never restores the old report');
  assert.equal(page.workers().length, 1);
  assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0, 'no report/session secrets in browser storage');
  assert.equal(new URL(page.url()).search, '', 'no report/session state in URL');
  report.pass('probe.page.lifecycle');
}
async function noSelectedFileFromPriorReport(page) {
  await check(page.locator('#selection-title')).not.toContainText(LONG_FILE_NAME);
  await check(page.locator('#details')).not.toContainText(LONG_FILE_NAME);
}

async function realJourney() {
  report.start('real.report.partial');
  const fixture = await startFixture();
  report.setStage('context_start');
  report.ensureActive();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  report.setStage('scenario');
  const traffic = await observe(context);
  const { page, errors } = await connect(context, fixture);
  assert.equal(traffic.count('resume'), 1, 'startup resumes once');
  assert.equal(traffic.count('pair'), 1, 'one explicit pairing request');
  await inspect(page, 'workspace');
  await check(page.locator('textarea, [contenteditable="true"]')).toHaveCount(0);
  await check(page.getByRole('button', { name: /^(Run|Debug|Terminal|Save file)$/i })).toHaveCount(0);
  await check(page.locator('#summary')).toContainText('1 traits');
  await check(page.locator('#summary')).toContainText('2 trait impls');
  await check(page.locator('#summary')).toContainText('1 free functions');
  const shapeCalls = traffic.count('inspect');
  await check(page.locator('#source-shape-summary')).toContainText('Partial');
  await check(page.locator('#source-shape-limits')).toContainText('not a complexity score');
  await check(page.locator('#source-shape')).toContainText('Cargo.toml directory (unverified)');
  const shapeTrait = page.locator('#source-shape [data-row-id^="shape-trait-"]:not([data-row-id*="-candidate-"]) [role="treeitem"]');
  await check(shapeTrait).toHaveCount(1);
  await check(shapeTrait).toContainText('View');
  await check(shapeTrait).toContainText('2 tentative candidate structs');
  const shapeCandidates = page.locator('#source-shape [data-row-id*="-candidate-"] [role="treeitem"]');
  await check(shapeCandidates).toHaveCount(2);
  await check(shapeCandidates.filter({ hasText: 'Widget' })).toHaveCount(1);
  const gadget = shapeCandidates.filter({ hasText: 'Gadget' });
  await gadget.focus();
  await page.keyboard.press('Enter');
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  await check(gadget).toBeFocused();
  await check(page.locator('#source-shape [role="treeitem"][aria-selected="true"]')).toHaveCount(1);
  await check(gadget).toHaveAttribute('aria-selected', 'false');
  await check(page.locator('#source-shape-details')).toContainText('Tentative local header impl blocks');
  await check(page.locator('#source-shape-details')).toContainText('Qualified-path impl headers (local/external unknown)');
  assert.equal(traffic.count('inspect'), shapeCalls, 'shape navigation is report-local');

  await check(page.locator('#files')).toContainText('README <safe>.md');
  await check(page.locator('#files')).toContainText('<svg onload=alert(1)>.txt');
  await check(page.locator('#files svg, #files script, #files img')).toHaveCount(0);
  if (process.platform !== 'win32') await check(page.locator('#files')).toContainText('unix bytes:');
  report.pass('real.report.partial');
  await keyboardAndFilters(page, traffic);
  await panesAndNarrow(page, traffic);
  await deviceViewportJourney(page, traffic);
  await reportAndPageLifecycle(page, traffic);
  report.start('real.report.empty');

  await inspect(page, 'empty', 'Inspection ready.');
  await check(page.locator('#summary')).toContainText('0 files');
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(1);
  await check(page.locator('#problems [role="treeitem"]')).toHaveCount(0);
  await screenshot(page, 'ide-workbench-empty.png');
  report.pass('real.report.empty');
  report.start('real.admission.denied');

  await inspect(page, 'denied', 'operation forbidden');
  await assertTone(page, 'status', 'error');
  await noReport(page);
  await check(page.locator('#inspect')).toBeEnabled();
  await check(page.locator('#pair-panel')).toBeHidden();
  await page.setViewportSize({ width: 760, height: 1000 });
  await screenshot(page, 'ide-workbench-narrow-denied.png');
  assert.deepEqual(errors, []);
  report.pass('real.admission.denied');
}

// Deterministic presentation-latency probe: queue real ordered WASM views, not
// model/reducer output replacements. A Worker channel is FIFO, so release them
// in exactly their original order; this is NOT a fabricated reorder scenario.
async function controlledInputDraftJourney(page) {
  report.start('probe.presentation.ordered_drafts');
  const worker = page.workers()[0];
  await worker.evaluate(() => {
    const original = self.postMessage;
    const queued = [];
    self.postMessage = function(message, ...rest) {
      if (message?.kind === 'view') {
        if (queued.length >= 8) throw new Error('Controlled presentation queue bound');
        queued.push([message, rest]);
        return;
      }
      return original.call(self, message, ...rest);
    };
    self.__forgeQueuedPresentation = {
      count() { return queued.length; },
      release() {
        const item = queued.shift();
        if (!item) throw new Error('Controlled presentation queue is empty');
        original.call(self, item[0], ...item[1]);
      },
      restore() {
        if (queued.length) throw new Error('Controlled presentation views remain unreleased');
        self.postMessage = original;
        delete self.__forgeQueuedPresentation;
      },
    };
  });
  const release = async () => {
    const before = await page.evaluate(() => window.__forgeBrowserProbe.receivedViews());
    await worker.evaluate(() => self.__forgeQueuedPresentation.release());
    await check.poll(() => page.evaluate(() => window.__forgeBrowserProbe.receivedViews())).toBe(before + 1);
  };
  const input = page.locator('#file-filter');
  await input.fill('f');
  await input.pressSequentially('o');
  await input.press('Backspace');
  await check.poll(() => worker.evaluate(() => self.__forgeQueuedPresentation.count())).toBe(3);
  await check(input).toHaveValue('f');
  await release(); // old f echo equals the newest draft, but is not its acknowledgement
  await check(input).toHaveValue('f');
  await release(); // old fo echo must not overwrite the newer f draft
  await check(input).toHaveValue('f');
  await release();
  await check(input).toHaveValue('f');
  await check(page.locator('#files')).toContainText('fault-fixture.txt');
  await input.fill('NOT-MATCH');
  await input.fill('');
  await check.poll(() => worker.evaluate(() => self.__forgeQueuedPresentation.count())).toBe(2);
  await release();
  await check(input).toHaveValue('');
  await release();
  await check(input).toHaveValue('');
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(2);
  await worker.evaluate(() => self.__forgeQueuedPresentation.restore());
  report.pass('probe.presentation.ordered_drafts');
}

async function controlledLifecycleJourney() {
  report.start('injected.requests.cancel_retry');
  // Explicit native injected-port fixture, NOT remote transport E2E evidence.
  // Real browser, generated worker/WASM, host admission and native timeout remain.
  const fixture = await startFixture(true);
  report.setStage('context_start');
  report.ensureActive();
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  report.setStage('scenario');
  const traffic = await observe(context);
  const { page, errors } = await connect(context, fixture);
  await page.locator('#project').selectOption('slow');
  const before = traffic.count('inspect');
  await page.locator('#inspect').evaluate(button => { button.click(); button.click(); button.click(); });
  await check(page.locator('#phase-label')).toContainText('Inspecting');
  await check(page.locator('#inspect')).toBeDisabled();
  await check(page.locator('#cancel')).toBeEnabled();
  await check(page.locator('#cancel')).toBeVisible();
  await page.locator('#cancel').click();
  await check(page.locator('#status')).toContainText('Stopped waiting locally.');
  await assertTone(page, 'status', 'warning');
  await check(page.locator('#cancel')).toBeHidden();
  await assertVisibleFocus(page);
  await delay(1000); // injected native port completes after local cancellation
  await check(page.locator('#status')).toContainText('Stopped waiting locally.');
  await noReport(page);
  assert.equal(traffic.count('inspect'), before + 1, 'rapid repeated Inspect creates one request');
  await inspect(page, 'workspace', 'Inspection ready.');
  await check(page.locator('#files')).toContainText('fault-fixture.txt');
  report.pass('injected.requests.cancel_retry');
  await controlledInputDraftJourney(page);
  report.start('injected.requests.revisions');

  await page.locator('#project').selectOption('slow');
  await page.locator('#inspect').click();
  await check(page.locator('#status')).toContainText('Inspecting');
  await page.locator('#project').selectOption('workspace');
  await page.locator('#project').selectOption('slow');
  await page.locator('#project').selectOption('workspace');
  await delay(1000);
  await check(page.locator('#status')).toContainText('Selection changed.');
  await noReport(page);
  report.pass('injected.requests.revisions');
  report.start('injected.requests.refresh_focus');

  await page.locator('#project').selectOption('slow');
  await page.locator('#inspect').click();
  await check(page.locator('#phase-label')).toContainText('Inspecting');
  const refreshRequests = traffic.count('inspect');
  const refreshResumes = traffic.count('resume');
  await page.reload({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#status')).toContainText('Connected.');
  await noReport(page);
  assert.equal(traffic.count('inspect'), refreshRequests, 'refresh during work never replays Inspect');
  assert.equal(traffic.count('resume'), refreshResumes + 1);
  assert.equal(page.workers().length, 1);
  await page.locator('#project').selectOption('slow');
  await page.locator('#inspect').click();
  await check(page.locator('#phase-label')).toContainText('Inspecting');
  await page.locator('#project').focus();
  await check(page.locator('#status')).toContainText('Inspection ready.');
  await check(page.locator('#project')).toBeFocused();
  report.pass('injected.requests.refresh_focus');
  report.start('injected.failures.unavailable_timeout');

  await inspect(page, 'unavailable', 'node observation unavailable');
  await noReport(page);
  await check(page.locator('#inspect')).toBeEnabled();
  const failedCount = traffic.count('inspect');
  await delay(250);
  assert.equal(traffic.count('inspect'), failedCount, 'unavailable does not auto-retry');
  await inspect(page, 'timeout', 'Deadline expired');
  await check(page.locator('#inspect')).toBeEnabled();
  await noReport(page);
  await delay(700); // allow non-preemptible injected port to finish
  await check(page.locator('#status')).toContainText('Deadline expired');
  await assertTone(page, 'status', 'error');
  await screenshot(page, 'ide-controlled-timeout.png');
  report.pass('injected.failures.unavailable_timeout');
  report.start('probe.worker.fatal');

  await inspect(page, 'workspace', 'Inspection ready.');
  // Controlled adapter fatal path, not a claim of spontaneous ABI failure.
  // Calls the real generated worker's fail-closed routine after real WASM data.
  const worker = page.workers()[0];
  await worker.evaluate(() => fatal('Controlled worker failure. Reload to retry.')).catch(error => {
    if (!/closed|destroyed|disposed/i.test(error.message)) throw error;
  });
  await check(page.locator('#runtime-message')).toContainText(/Reload/i);
  await noReport(page);
  await check(page.locator('#project option')).toHaveCount(0);
  await check(page.locator('#node option')).toHaveCount(0);
  assert.equal(await page.locator('select[data-action]').evaluateAll(selects => selects.every(select => !select.title)), true, 'fatal clears protected select titles');
  await check(page.locator('#workbench')).toBeHidden();
  await check(page.locator('#inspect')).toBeDisabled();
  await check.poll(() => page.workers().length).toBe(0);
  await screenshot(page, 'ide-controlled-fatal.png');
  report.pass('probe.worker.fatal');
  report.start('injected.catalog.empty');
  await page.reload({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#status')).toContainText('Connected.');
  await noReport(page);
  assert.equal(page.workers().length, 1);

  // Production HostConfig deliberately forbids a node with zero configured
  // projects. This separately labeled catalog-only fixture changes a successful
  // Resume response for UI acceptance; it is NOT host/agent authorization proof.
  await context.route('**/forge/v1/dispatch', async route => {
    if (route.request().postDataJSON()?.kind !== 'resume') return route.continue();
    const response = await route.fetch();
    const body = await response.json();
    assert.equal(body.event.kind, 'paired');
    for (const node of body.event.catalog.nodes) node.projects = [];
    await route.fulfill({ response, json: body });
  });
  await page.reload({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#status')).toContainText('Connected.');
  await check(page.locator('#workbench')).toContainText(/No allowed project/i);
  await check(page.locator('#inspect')).toBeDisabled();
  await check(page.locator('#project option')).toHaveCount(0);
  await check(page.locator('#phase-label')).toHaveText('No allowed project');
  await assertTone(page, 'phase-label', 'warning');
  await noReport(page);
  await screenshot(page, 'ide-controlled-no-allowed-project.png');
  await context.unroute('**/forge/v1/dispatch');
  report.pass('injected.catalog.empty');
  report.start('probe.session.missing');

  await context.clearCookies();
  await page.reload({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#pair')).toBeEnabled();
  await check(page.locator('#connection')).toBeVisible();
  await check(page.locator('#workbench')).toBeHidden();
  await check(page.locator('#project option')).toHaveCount(0);
  await check(page.locator('#node option')).toHaveCount(0);
  await noReport(page);
  assert.deepEqual(errors, []);
  report.pass('probe.session.missing');
}
// Hash only fixed generated assets and the two explicit executable inputs. Never
// retain paths or bytes; fingerprints make standalone dirty-tree runs auditable.
async function sha256(path) {
  const hash = createHash('sha256');
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest('hex');
}
const passed = await runHarness({
  args: process.argv.slice(2),
  async setup(currentReport, directory) {
    report = currentReport;
    output = directory;
    report.setStage('dependency');
    const { chromium, expect, devices } = await import('@playwright/test');
    verifyViewportProfiles(devices);
    check = expect.configure({ timeout: 10_000 });
    report.setStage('input_validation');
    [assets, agent, fixtureBinary] = await Promise.all(process.argv.slice(2, 5).map(path => realpath(path)));
    assert.equal((await stat(assets)).isDirectory(), true);
    for (const path of [agent, fixtureBinary]) assert.equal((await stat(path)).isFile(), true);
    report.setStage('fingerprints');
    const names = ['index.html', 'style.css', 'main.js', 'worker.js', 'app.wasm', 'manifest.json'];
    const hashes = await Promise.all(names.map(name => sha256(join(assets, name))));
    const fingerprints = Object.fromEntries(names.map((name, index) => [name, hashes[index]]));
    fingerprints.agent_binary = await sha256(agent);
    fingerprints.fixture_binary = await sha256(fixtureBinary);
    report.addFingerprints(fingerprints);
    report.setStage('browser_channel');
    const channel = process.env.FORGE_UI_BROWSER_CHANNEL ?? 'chrome';
    assert.equal(channel, 'chrome');
    if (report.frozen) throw new Error('Journey stopped before launch');
    report.setStage('browser_launch');
    // Sandbox remains mandatory. No security overrides, fake WASM, deployment or
    // user profile. Only the labeled empty-catalog case intercepts HTTP.
    browserOwner = acquireOwned(
      () => chromium.launch({ headless: true, chromiumSandbox: true, channel }),
      ownedBrowser => ownedBrowser.close(),
    );
    browser = await browserOwner.ready;
    report.browserVersion(browser.version());
    report.ensureActive();
  },
  async journey() {
    await realJourney();
    await controlledLifecycleJourney();
  },
  diagnose: (currentReport, directory) => captureFailureScreenshot(currentReport, directory, [...active], process.env.FORGE_UI_QA_FAILURE_SCREENSHOT === '1'),
  async cleanup(currentReport) {
    await cleanupResources([
      ...(browserOwner ? [{ name: 'browser', close: () => browserOwner.close() }] : []),
      ...[...active].map(fixture => ({ name: fixture.name, close: () => stopFixture(fixture) })),
    ], currentReport);
  },
});
if (!passed) process.exitCode = 1;
