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

async function startFixture(faults = false, singleProject = false, managedWrites = false) {
  report.ensureActive();
  report.setStage('fixture_start');
  const mode = faults ? ['--faults'] : managedWrites ? ['--managed-writes'] : singleProject ? ['--single-project'] : [];
  const child = spawn(fixtureBinary, [assets, agent, ...mode], {
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
    if (!['resume', 'pair', 'inspect', 'source'].includes(kind)) kind = 'other';
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
async function connect(context, fixture, autoLoad = false) {
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
  await check(page.locator('#chat')).toBeVisible();
  await check(page.locator('#workbench')).toBeHidden();
  await page.locator('#open-workbench').click();
  await check(page.locator('#status')).toContainText(autoLoad ? 'Partial inspection:' : 'Connected.');
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
  const narrow = await page.locator('#workspace').getAttribute('data-narrow') === 'true';
  const projectionVisible = narrow || (after && tree && after.y >= tree.y - 1 && after.y + after.height <= tree.y + tree.height + 1);
  if (!projectionVisible) process.stderr.write('QA_CHECK pointer_projection_visibility_failed\n');
  assert.ok(projectionVisible, 'selected row stays inside its scrolling tree after projection');
  await check(page.locator('#selection-title')).toContainText(name);
  if (narrow) await check(page.locator('#workspace-panel')).toBeVisible();
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
  await check(page.locator('#source-view')).toHaveText('');
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

async function disclosure(page, id, open) {
  const details = page.locator(`#${id}`);
  if (await details.evaluate(node => node.open) !== open) {
    const summary = page.locator(`#${id} > summary`);
    await summary.focus();
    await page.keyboard.press('Enter');
    await check(summary).toBeFocused();
  }
  await check(details).toHaveJSProperty('open', open);
}
async function ensurePane(page, id) {
  if (!await page.locator(`#${id}`).isVisible()) await page.locator(`#${id}-switch`).click();
  await check(page.locator(`#${id}`)).toBeVisible();
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
  await page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ }).click();
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
  await ensurePane(page, 'problems-panel');
  await check(page.locator('#outline-panel')).toBeHidden();
  const problem = page.locator('#problems [role="treeitem"]').filter({ hasText: 'broken.rs' }).first();
  await check(problem).toBeVisible();
  await problem.click();
  await check(page.locator('#selection-title')).toContainText('broken.rs');
  await check(page.locator('#details')).toContainText(/diagnostic/i);
  await check(page.locator('#declaration-details')).not.toContainText('inspect_me');
  assert.equal(traffic.count('inspect'), calls, 'navigation/filter/page/problem actions never repeat project inspection');
  report.pass('real.pagination.problems');
}

async function panesAndNarrow(page, traffic) {
  report.start('real.panes.desktop');
  const calls = traffic.count('inspect');
  await filterFiles(page, 'lib.rs');
  await selectFile(page, 'lib.rs');
  await ensurePane(page, 'outline-panel');
  await check(page.locator('#problems-panel')).toBeHidden();
  await disclosure(page, 'source-shape-panel', false);
  await disclosure(page, 'file-metadata-panel', false);
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
    await ensurePane(page, pane);
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
    await ensurePane(page, pane);
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
    await ensurePane(page, pane);
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
  await check(page.locator('#source-view [data-line="6"][data-highlight="true"]')).toBeInViewport();
  await screenshot(page, 'ide-workbench-narrow-declaration.png');
  await page.locator('#problems-panel-switch').click();
  await page.locator('#problems [role="treeitem"]').filter({ hasText: 'broken.rs' }).first().click();
  await page.locator('#workspace-panel-switch').click();
  await check(page.locator('#selection-title')).toContainText('broken.rs');
  await check(page.locator('#details')).toContainText(/diagnostic/i);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await check(page.locator('#workspace-panel')).toBeVisible();
  await check(page.locator('#explorer-panel')).toBeVisible();
  await check(page.locator('#outline-panel')).toBeHidden();
  await check(page.locator('#problems-panel')).toBeVisible();
  await check(page.locator('#selection-title')).toContainText('broken.rs');
  assert.equal(page.workers().length, 1, 'pane changes never remount the worker');
  assert.equal(traffic.count('inspect'), calls, 'pane changes never inspect remotely');
  await screenshot(page, 'ide-workbench-partial-problems.png');
  report.pass('real.panes.narrow');
}

// Reuse the accepted real-agent fixture across fixed CSS viewport profiles. No
// new inspection, device user agent, touch flags or WebKit claims are introduced.
async function deviceViewportJourney(page, traffic) {
  const calls = traffic.count('inspect');
  async function assertViewportFocus() {
    const scenario = report.active?.id;
    const checkpoint = report.active?.checkpoint;
    await assertVisibleFocus(page);
    await check.poll(async () => {
      const sample = await page.evaluate(() => {
        const active = document.activeElement;
        const rect = active.getBoundingClientRect();
        const owner = active.closest('.fg-pane-body')?.getBoundingClientRect();
        const matches = rect.left >= Math.max(0, owner?.left ?? 0) - 1 &&
          rect.right <= Math.min(innerWidth, owner?.right ?? innerWidth) + 1 &&
          rect.top >= Math.max(0, owner?.top ?? 0) - 1 &&
          rect.bottom <= Math.min(innerHeight, owner?.bottom ?? innerHeight) + 1;
        const rectangle = value => ({ left: value.left, top: value.top, right: value.right, bottom: value.bottom });
        return { matches, geometry: { focused: rectangle(rect), pane: owner ? rectangle(owner) : null, viewport: { width: innerWidth, height: innerHeight } } };
      });
      report.focusGeometry(scenario, checkpoint, sample.geometry);
      return sample.matches;
    }).toBe(true);
  }
  for (const viewport of VIEWPORTS) {
    report.start(viewport.id);
    report.checkpoint('viewport_resize');
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    report.checkpoint('viewport_dimensions');
    assert.deepEqual(await page.evaluate(() => ({ width: innerWidth, height: innerHeight })), { width: viewport.width, height: viewport.height });
    const narrow = viewport.width <= 800;
    report.checkpoint('viewport_breakpoint');
    await check(page.locator('#workspace')).toHaveAttribute('data-narrow', String(narrow));
    if (narrow) {
      const projectControls = page.locator('#project-controls-toggle');
      const closed = await projectControls.isVisible() && await projectControls.getAttribute('aria-expanded') === 'false';
      if (closed) await projectControls.click();
      assert.equal(await page.locator('#workbench .fg-toolbar select').evaluateAll(fields => fields.every(field => field.getBoundingClientRect().width >= 120)), true, 'narrow context selectors stay readable rather than shrinking around status');
      if (closed) await projectControls.click();
    }
    async function activate(pane, checkpoint) {
      report.checkpoint(checkpoint);
      if (narrow) {
        const tab = page.locator(`#${pane}-switch`);
        await tab.focus();
        await page.keyboard.press('Enter');
        await check(tab).toHaveAttribute('aria-pressed', 'true');
        for (const other of ['explorer-panel', 'outline-panel', 'problems-panel', 'workspace-panel'].filter(id => id !== pane)) {
          await check(page.locator(`#${other}`)).toBeHidden();
        }
      } else {
        await ensurePane(page, pane);
        await page.locator(`#${pane}-heading`).focus();
      }
      await check(page.locator(`#${pane}`)).toBeVisible();
      await assertViewportFocus();
    }
    await activate('explorer-panel', 'explorer_activate');
    report.checkpoint('file_filter');
    await filterFiles(page, 'lib.rs');
    report.checkpoint('file_select');
    await selectFile(page, 'lib.rs');
    await activate('workspace-panel', 'inventory_activate');
    await disclosure(page, 'source-shape-panel', true);
    report.checkpoint('inventory_caveat');
    await check(page.locator('#source-shape-limits')).toContainText('not a complexity score');
    const candidate = page.locator('#source-shape [data-row-id*="-candidate-"] [role="treeitem"]').filter({ hasText: 'Gadget' });
    report.checkpoint('candidate_select');
    await candidate.scrollIntoViewIfNeeded();
    await candidate.focus();
    await page.keyboard.press('Enter');
    report.checkpoint('candidate_focus');
    await check(candidate).toBeFocused();
    await assertViewportFocus();
    report.checkpoint('candidate_selection');
    await check(page.locator('#selection-title')).toContainText('lib.rs');
    await activate('outline-panel', 'outline_activate');
    const symbol = page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ });
    report.checkpoint('symbol_select');
    await symbol.focus();
    await page.keyboard.press('Enter');
    report.checkpoint('symbol_focus');
    await assertViewportFocus();
    await activate('workspace-panel', 'details_activate');
    report.checkpoint('details_content');
    await check(page.locator('#declaration-details')).toContainText('inspect_me');
    report.checkpoint('details_scroll');
    await check(page.locator('#source-view [data-line="6"]')).toHaveAttribute('data-highlight', 'true');
    // Low landscape heights legitimately scroll the page vertically. The selected
    // declaration remains reachable and readable without horizontal scrolling.
    report.checkpoint('details_readable');
    const selectedName = page.locator('#source-view [data-line="6"] .fg-source-text');
    await selectedName.scrollIntoViewIfNeeded();
    assert.equal(await selectedName.evaluate(node => {
      const rect = node.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0 && rect.left >= -1 && rect.right <= innerWidth + 1 && rect.top >= -1 && rect.bottom <= innerHeight + 1;
    }), true, 'selected declaration source is readable at this viewport');
    await activate('outline-panel', 'outline_return');
    report.checkpoint('outline_restore');
    if (narrow) await check(symbol).toBeFocused();
    await symbol.focus();
    // The tree may be the last tabbable control in narrow mode. Test exit and
    // reentry through its preceding filter, without requiring a focus trap.
    report.checkpoint('filter_keyboard_exit');
    await page.keyboard.press('Shift+Tab');
    await check(page.locator('#symbol-filter')).toBeFocused();
    await assertViewportFocus();
    report.checkpoint('symbol_keyboard_return');
    await page.keyboard.press('Tab');
    await check(symbol).toBeFocused();
    await assertViewportFocus();
    await activate('workspace-panel', 'workspace_return');
    report.checkpoint('horizontal_overflow');
    assert.equal(await page.locator('#workspace-panel .fg-pane-body').evaluate(body => body.scrollWidth <= body.clientWidth + 1), true, 'main pane has no horizontal overflow');
    report.checkpoint('worker_lifetime');
    assert.equal(page.workers().length, 1, 'viewport changes keep one worker');
    report.checkpoint('local_requests');
    assert.equal(traffic.count('inspect'), calls, 'viewport navigation remains report-local');
    await disclosure(page, 'source-shape-panel', false);
    await page.locator('#source-view [data-line="6"] .fg-source-number').focus();
    await selectedName.scrollIntoViewIfNeeded();
    await check(selectedName).toBeInViewport();
    await assertViewportFocus();
    report.checkpoint('viewport_screenshot');
    await screenshot(page, viewport.screenshot);
    report.pass(viewport.id);
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  await check(page.locator('#workspace')).toHaveAttribute('data-narrow', 'false');
}

// Controlled FIFO latency on the real worker's accepted WASM views, not fake
// model state. Deliberately restore an old main-pane row before external intent
// is presented, then require the renderer to use dispatch provenance.
async function delayedPaneContextProbe(page, traffic) {
  const calls = traffic.count('inspect');
  await page.setViewportSize({ width: 667, height: 375 });
  await page.locator('#explorer-panel-switch').click();
  await filterFiles(page, 'lib.rs');
  await selectFile(page, 'lib.rs');
  await check(page.locator('#declaration-details')).not.toContainText('inspect_me');
  await page.locator('#workspace-panel-switch').click();
  await disclosure(page, 'source-shape-panel', true);
  const candidate = page.locator('#source-shape [data-row-id*="-candidate-"] [role="treeitem"]').filter({ hasText: 'Gadget' });
  await candidate.scrollIntoViewIfNeeded();
  await candidate.focus();
  const directBefore = await page.evaluate(() => window.__forgeBrowserProbe.receivedViews());
  await page.keyboard.press('Enter');
  await check.poll(() => page.evaluate(() => window.__forgeBrowserProbe.receivedViews())).toBe(directBefore + 1);
  await check(candidate).toBeFocused();
  await page.locator('#outline-panel-switch').click();
  const worker = page.workers()[0];
  await worker.evaluate(() => {
    const original = self.postMessage;
    const queue = [];
    self.postMessage = function(message, ...rest) {
      if (message?.kind === 'view') {
        if (queue.length >= 8) throw new Error('Controlled pane presentation queue bound');
        queue.push([message, rest]);
        return;
      }
      return original.call(self, message, ...rest);
    };
    self.__forgePanePresentation = {
      count() { return queue.length; },
      release() {
        const item = queue.shift();
        if (!item) throw new Error('Controlled pane presentation queue empty');
        original.call(self, item[0], ...item[1]);
      },
      restore() {
        if (queue.length) throw new Error('Controlled pane presentations remain');
        self.postMessage = original;
        delete self.__forgePanePresentation;
      },
    };
  });
  const symbol = page.locator('#outline').getByRole('treeitem', { name: /^inspect_me\b/ });
  await symbol.focus();
  await page.keyboard.press('Enter');
  await check.poll(() => worker.evaluate(() => self.__forgePanePresentation.count())).toBe(1);
  await page.locator('#workspace-panel-switch').click();
  await check(candidate).toBeFocused();
  await check.poll(() => page.locator('#workspace-panel .fg-pane-body').evaluate(body => body.scrollTop)).toBeGreaterThan(0);
  const before = await page.evaluate(() => window.__forgeBrowserProbe.receivedViews());
  await worker.evaluate(() => self.__forgePanePresentation.release());
  await check.poll(() => page.evaluate(() => window.__forgeBrowserProbe.receivedViews())).toBe(before + 1);
  await check(page.locator('#declaration-details')).toContainText('inspect_me');
  const revealedLine = page.locator('#source-view [data-line="6"]');
  await check(revealedLine).toHaveAttribute('data-highlight', 'true');
  await check(revealedLine.locator('.fg-source-number')).toBeFocused();
  await check(revealedLine.locator('.fg-source-number')).toBeInViewport();
  await worker.evaluate(() => self.__forgePanePresentation.restore());
  // The converse is equally important: a direct in-pane context-changing action
  // retains its clicked row instead of jumping to the pane heading.
  await candidate.scrollIntoViewIfNeeded();
  await candidate.focus();
  await page.keyboard.press('Enter');
  await check(page.locator('#declaration-details')).not.toContainText('inspect_me');
  await check(candidate).toBeFocused();
  await check.poll(() => page.locator('#workspace-panel .fg-pane-body').evaluate(body => body.scrollTop)).toBeGreaterThan(0);
  assert.equal(traffic.count('inspect'), calls, 'delayed pane projection remains local');
  assert.equal(page.workers().length, 1);
  await page.setViewportSize({ width: 1440, height: 1000 });
}

async function reportAndPageLifecycle(page, traffic) {
  report.start('probe.report.stale_actions');
  await delayedPaneContextProbe(page, traffic);
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
      disclosuresClosed: [...document.querySelectorAll('.fg-disclosure')].every(node => !node.open),
      hidden: document.getElementById('workbench').hidden,
    };
  });
  assert.equal(cleared.options, 0, 'teardown synchronously clears protected catalog DOM');
  assert.equal(cleared.titlesCleared, true, 'teardown also removes catalog metadata from select titles');
  assert.equal(cleared.rows, 0, 'teardown synchronously clears report rows');
  assert.equal(cleared.details, '', 'teardown synchronously clears metadata');
  assert.equal(cleared.declarations, '', 'teardown synchronously clears declaration metadata');
  assert.equal(cleared.disclosuresClosed, true, 'teardown closes disclosures as well as clearing their protected contents');
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
  await check(page.locator('#chat')).toBeVisible();
  await page.locator('#open-workbench').click();
  assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0, 'no report/session secrets in browser storage');
  assert.equal(new URL(page.url()).search, '', 'no report/session state in URL');
  report.pass('probe.page.lifecycle');
}
async function noSelectedFileFromPriorReport(page) {
  await check(page.locator('#selection-title')).not.toContainText(LONG_FILE_NAME);
  await check(page.locator('#details')).not.toContainText(LONG_FILE_NAME);
}

// Actual mTLS source reads, including the single-project phone entry path.
// Search is report-local; opening a result obtains its digest-checked source.
async function sourceSearchJourney() {
  const managedWrites = process.platform !== 'win32' && process.getuid?.() !== 0;
  const fixture = await startFixture(false, true, managedWrites);
  report.setStage('scenario');
  const context = await browser.newContext({ viewport: { width: 390, height: 664 } });
  try {
    const traffic = await observe(context);
    const { page, errors } = await connect(context, fixture, true);
    process.stderr.write('QA_CHECK source_search_connected\n');
    assert.equal(traffic.count('inspect'), 1, 'one admitted project opens automatically');
    await check(page.locator('#explorer-panel')).toBeVisible();
    process.stderr.write('QA_CHECK source_search_explorer\n');
    const reads = traffic.count('source');
    await page.locator('#project-query').fill('INSPECT_ME');
    const match = page.locator('#search-results [role="treeitem"]').filter({ hasText: 'inspect_me' });
    await check(match).toHaveCount(1);
    process.stderr.write('QA_CHECK source_search_match\n');
    await check(match).toContainText(/src\s*\/\s*lib\.rs/);
    assert.equal(traffic.count('source'), reads, 'searching symbols does not read files');
    await match.focus();
    await page.keyboard.press('Enter');
    await check(page.locator('#workspace-panel')).toBeVisible();
    process.stderr.write('QA_CHECK source_search_selected\n');
    await check(page.locator('#source-view')).toContainText('pub fn inspect_me() {}');
    process.stderr.write('QA_CHECK source_search_read\n');
    await check(page.locator('#source-view [data-line="6"]')).toHaveAttribute('data-highlight', 'true');
    await check(page.locator('#source-view')).not.toHaveAttribute('contenteditable', 'true');
    assert.equal(traffic.count('source'), reads + 1, 'opening a search result reads one file');
    assert.equal(traffic.count('inspect'), 1, 'opening source does not repeat inspection');
    await assertVisibleFocus(page);
    const contextualRequests = [];
    const gateway = '**/forge/v1/dispatch';
    let releasePreview;
    const deferredPreview = new Promise(resolve => { releasePreview = resolve; });
    const contextualChat = async route => {
      const body = route.request().postDataJSON();
      if (body?.kind === 'propose_edit') {
        if (body.request.messages.at(-1).content === 'Delay this preview.') await deferredPreview;
        return route.fulfill({ json: {
          protocol_version: 1,
          event: { kind: 'edit_result', request_id: body.request.request_id,
            outcome: { kind: 'preview', preview: {
              base_sha256: body.request.file_context.sha256,
              lines: [
                { old_line: 6, new_line: null, text: 'pub fn inspect_me() {}' },
                { old_line: null, new_line: 6, text: 'pub fn inspect_me() { /* <script> */ }' },
              ],
            } } },
        } });
      }
      if (body?.kind !== 'chat' || !body.request.file_context) return route.continue();
      contextualRequests.push(body.request);
      await route.fulfill({ json: {
        protocol_version: 1,
        event: { kind: 'chat_result', request_id: body.request.request_id,
          outcome: { kind: 'answer', content: 'The selected Rust fixture contains inspect_me.' } },
      } });
    };
    await context.route(gateway, contextualChat);
    try {
      const dock = page.locator('#workbench-chat');
      const prompt = dock.getByRole('textbox', { name: 'Ask Forge about this file' });
      await check(dock.locator('#workbench-chat-context')).toHaveText(/^Lines 1–6 · rev [0-9a-f]{8}$/);
      await prompt.fill('What is defined here?');
      await dock.getByRole('button', { name: 'Send' }).click();
      await check(dock.getByRole('status')).toHaveText('Reply ready in Chat.');
      assert.equal(contextualRequests.length, 1);
      assert.equal(contextualRequests[0].file_context.byte_start, 0);
      assert.match(contextualRequests[0].file_context.sha256, /^[0-9a-f]{64}$/);
      assert.deepEqual(contextualRequests[0].messages, [{ role: 'user', content: 'What is defined here?' }]);
      await page.locator('#open-chat').click();
      await check(page.locator('#chat-transcript')).toContainText('The selected Rust fixture contains inspect_me.');
      await page.locator('#open-workbench').click();
      await check(dock).toBeVisible();
      const readsBeforePreview = traffic.count('source');
      await prompt.fill('Add a comment to inspect_me.');
      await dock.getByRole('button', { name: 'Propose edit' }).click();
      const diff = page.getByRole('region', { name: 'Proposed diff' });
      await check(diff.locator('[data-change=removed]')).toContainText('−');
      await check(diff.locator('[data-change=added]')).toContainText('<script>');
      await check(diff.locator('script')).toHaveCount(0);
      await check(page.locator('#source-view')).toBeHidden();
      await page.getByRole('button', { name: 'Wrap', exact: true }).click();
      await check(page.locator('#source-view')).toHaveAttribute('data-wrap', 'true');
      await page.getByRole('button', { name: 'Wrap', exact: true }).click();
      await dock.getByRole('button', { name: 'Discard' }).click();
      await check(diff).toBeHidden();
      await check(page.locator('#source-view [data-line="6"] .fg-source-text')).toHaveText('pub fn inspect_me() {}');
      assert.equal(traffic.count('source'), readsBeforePreview, 'preview and discard leave verified source intact');
      await prompt.fill('Delay this preview.');
      await dock.getByRole('button', { name: 'Propose edit' }).click();
      await check(dock.locator('#workbench-chat-status')).toHaveText('Preparing proposal…');
      await page.getByRole('button', { name: 'Back to files', exact: true }).click();
      await page.locator('#project-query').fill('README <safe>');
      await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'README <safe>.md' }).click();
      await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
      releasePreview();
      await check(dock.locator('#workbench-chat-status')).toHaveText('Reply ready in Chat.');
      await check(diff).toBeHidden();
    } finally {
      releasePreview();
      await context.unroute(gateway, contextualChat);
    }
    const projectControls = page.getByRole('button', { name: 'Project controls' });
    const viewOptions = page.locator('#workspace-options > summary');
    await check(projectControls).toHaveAttribute('aria-expanded', 'false');
    await check(page.locator('#project')).toBeHidden();
    await check(viewOptions).toBeHidden();
    assert.equal(await page.evaluate(() => {
      const rect = selector => document.querySelector(selector).getBoundingClientRect();
      const project = rect('#project-controls-toggle');
      const tabs = [...document.querySelectorAll('.fg-pane-switches button')].map(tab => tab.getBoundingClientRect());
      return Math.abs(rect('.fg-header').top - rect('#ide-navigation').top) <= 1 &&
        tabs.every(tab => tab.width >= 44 && tab.height >= 44 &&
          Math.abs(tab.top + tab.height / 2 - project.top - project.height / 2) <= 1) &&
        project.width >= 44 && project.height >= 44;
    }), true, 'mobile file chrome shares two rows above the file heading with 44px pane controls');
    await projectControls.press('Enter');
    await check(projectControls).toHaveAttribute('aria-expanded', 'true');
    await check(page.locator('#project')).toBeVisible();
    await check(viewOptions).toBeVisible();
    await viewOptions.press('Enter');
    await check(page.locator('#workspace-reset')).toBeVisible();
    await viewOptions.press('Enter');
    await projectControls.press('Enter');
    await check(page.locator('#project')).toBeHidden();
    await check(viewOptions).toBeHidden();
    await check(page.locator('#source-pagination')).toBeHidden();
    assert.equal(traffic.count('inspect'), 1, 'project disclosure stays local');
    await check(page.locator('#workspace-panel')).toBeVisible();
    await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
    await check(page.locator('#source-view script, #source-view img, #source-view svg')).toHaveCount(0);
    await page.getByRole('button', { name: 'Back to files', exact: true }).click();
    await check(page.locator('#explorer-panel')).toBeVisible();
    await page.locator('#project-query').fill('paged.txt');
    await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'paged.txt' }).click();
    await check(page.locator('#source-view')).toContainText('Source line 1:');
    await check(page.locator('#source-view .fg-source-line')).toHaveCount(256);
    const pageReads = traffic.count('source');
    const sourceFrame = page.locator('[data-source-frame]');
    const source = page.locator('#source-view');
    const wrap = page.getByRole('button', { name: 'Wrap', exact: true });
    await check(sourceFrame).toHaveAttribute('data-overflow-right', '');
    await source.evaluate(node => { node.scrollLeft = node.scrollWidth; });
    await check(sourceFrame).not.toHaveAttribute('data-overflow-right', '');
    await wrap.press('Space');
    await check(wrap).toHaveAttribute('aria-pressed', 'true');
    assert.equal(await source.evaluate(node => node.scrollWidth <= node.clientWidth + 1 && node.scrollLeft === 0), true);
    await check(source.locator('[data-line="1"] .fg-source-number')).toHaveText('1');
    await wrap.press('Space');
    await check(wrap).toHaveAttribute('aria-pressed', 'false');
    await check(sourceFrame).toHaveAttribute('data-overflow-right', '');
    assert.equal(traffic.count('source'), pageReads, 'wrapping reuses the accepted source window');
    await page.locator('#source-more').click();
    await check(page.locator('#source-view')).toContainText('Source line 257:');
    await check(page.locator('#source-more')).toBeFocused();
    await check(page.locator('#source-more')).toBeInViewport();
    await page.locator('#source-more').click();
    await check(page.locator('#source-view')).toContainText('Source line 600:');
    await check(page.locator('#source-more')).toBeDisabled();
    await page.locator('#source-previous').click();
    await check(page.locator('#source-view')).toContainText('Source line 257:');
    assert.equal(traffic.count('source'), pageReads, 'source paging reuses the exact accepted file');
    await page.getByRole('button', { name: 'Back to files', exact: true }).click();
    await page.locator('#project-query').fill('late_view');
    await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'late_view' }).click();
    const lateLine = page.locator('#source-view [data-line="300"]');
    await check(lateLine).toHaveAttribute('data-highlight', 'true');
    await check.poll(() => lateLine.evaluate(line => {
      const source = line.closest('[data-source-view]');
      return Math.abs(line.getBoundingClientRect().top - source.getBoundingClientRect().top - source.clientTop);
    })).toBeLessThan(3);
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    await check(page.locator('#source-view [data-line="301"]')).toBeInViewport();
    if (managedWrites) {
      await page.setViewportSize({ width: 390, height: 844 });
      await managedWriteJourney(page, context);
      await page.setViewportSize({ width: 390, height: 664 });
    }
    const beforeReload = traffic.count('inspect');
    await page.reload({ waitUntil: 'domcontentloaded' });
    await check(page.locator('#chat')).toBeVisible();
    await page.locator('#open-workbench').click();
    await check(page.locator('#status')).toContainText('Partial inspection:');
    await check(page.locator('#source-view')).toHaveText('');
    assert.equal(traffic.count('inspect'), beforeReload + 1, 'reload obtains a fresh single-project report');
    assert.deepEqual(errors, []);
  } catch (error) {
    const location = String(error.stack).match(/journey\.mjs:(\d+):(\d+)/);
    if (location) process.stderr.write(`QA_CHECK source_journey_location=${location[1]}:${location[2]}\n`);
    throw error;
  } finally {
    await context.close();
    await stopFixture(fixture);
  }
}

async function managedWriteJourney(page, context) {
  await page.getByRole('button', { name: 'Back to files', exact: true }).click();
  await page.locator('#project-query').fill('managed.txt');
  await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'managed.txt' }).click();
  const source = page.locator('#source-view');
  const dock = page.locator('#workbench-chat');
  await check(source).toContainText('old line');
  await dock.getByRole('textbox', { name: 'Ask Forge about this file' }).fill('Update the second line.');
  await dock.getByRole('button', { name: 'Propose edit' }).click();
  const diff = page.getByRole('region', { name: 'Proposed diff' });
  await check(diff).toContainText('extra line');
  await screenshot(page, 'ide-workbench-managed-preview.png');

  // Let the real host and mTLS agent complete the write, then drop the reply
  // before it reaches the browser. Status must recover the same operation.
  const gateway = '**/forge/v1/dispatch';
  const operations = [];
  const loseApplyReply = async route => {
    const body = route.request().postDataJSON();
    if (body?.kind !== 'edit_write') return route.continue();
    operations.push([body.request.intent.kind, body.request.operation]);
    if (body.request.intent.kind !== 'apply') return route.continue();
    const response = await route.fetch();
    assert.equal((await response.json()).event.outcome.kind, 'applied');
    return route.abort('failed');
  };
  await context.route(gateway, loseApplyReply);
  try {
    await dock.getByRole('button', { name: 'Apply', exact: true }).click();
    await check(dock.locator('#workbench-write-status')).toContainText('Write outcome unknown');
    await dock.getByRole('button', { name: 'Check status', exact: true }).click();
    await check(source.locator('[data-line="3"]')).toContainText('extra line');
    assert.deepEqual(operations.map(([kind]) => kind), ['apply', 'status']);
    assert.equal(operations[0][1], operations[1][1]);
    await screenshot(page, 'ide-workbench-managed-applied.png');
  } finally {
    await context.unroute(gateway, loseApplyReply);
  }

  await page.locator('#open-chat').click();
  await check(page.locator('#chat-transcript')).toContainText(/Applied · rev [0-9a-f]{8}/);
  await page.locator('#chat-draft').fill('What changed in the file?');
  await page.getByRole('button', { name: 'Send with file', exact: true }).click();
  await check(page.locator('#chat-transcript')).toContainText('The verified excerpt contains the updated and extra lines.');
  await page.reload({ waitUntil: 'domcontentloaded' });
  await check(page.locator('#chat')).toBeVisible();
  await page.locator('#open-workbench').click();
  await page.locator('#project-query').fill('managed.txt');
  await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'managed.txt' }).click();
  await check(source.locator('[data-line="3"]')).toContainText('extra line');
  await check(dock.locator('#workbench-write-status')).toContainText(/Applied · rev [0-9a-f]{8}/);
  await dock.getByRole('button', { name: 'Undo edit', exact: true }).click();
  await check(source.locator('[data-line="2"]')).toContainText('old line');
  await check(source.locator('[data-line="3"]')).toContainText('omega');
  await check(dock.locator('#workbench-write-status')).toContainText(/Reverted · rev [0-9a-f]{8}/);
}

async function chatJourney(page, context) {
  const gateway = '**/forge/v1/dispatch';
  const transcript = page.locator('#chat-transcript');
  const draft = page.locator('#chat-draft');
  const select = page.locator('#chat-conversation');
  const send = page.locator('#send-chat');
  const sent = [];
  let release;
  const hold = new Promise(resolve => { release = resolve; });
  const routeChat = async route => {
    const body = route.request().postDataJSON();
    if (body?.kind !== 'chat') return route.continue();
    sent.push(body.request.messages);
    if (sent.length === 1) await hold;
    await route.fulfill({ json: {
      protocol_version: 1,
      event: {
        kind: 'chat_result', request_id: body.request.request_id,
        outcome: { kind: 'answer', content: 'Reply <img src=x onerror=alert(1)>' },
      },
    } });
  };
  await page.locator('#open-chat').click();
  await check(page.locator('#chat')).toBeVisible();
  await draft.fill('Retry this draft');
  await send.click();
  await check(page.locator('#chat-status')).toContainText('inference is unavailable');
  await check(draft).toHaveValue('Retry this draft');
  await context.route(gateway, routeChat);
  try {
    await draft.fill('Original conversation');
    await send.click();
    await check(transcript).toContainText('Original conversation');
    await page.locator('#new-chat').click();
    await check(select).toHaveValue('1');
    await check(transcript).not.toContainText('Original conversation');
    await draft.fill('Draft for the other conversation');
    release();
    await check(send).toBeEnabled();
    await check(transcript).not.toContainText('Reply');
    await select.selectOption('0');
    await check(transcript).toContainText('Reply <img src=x onerror=alert(1)>');
    await check(transcript.locator('img,script')).toHaveCount(0);
    await draft.fill('Follow-up');
    await send.click();
    await check(transcript.locator('[data-role=assistant]')).toHaveCount(2);
    assert.deepEqual(sent[1].map(message => message.content), [
      'Original conversation', 'Reply <img src=x onerror=alert(1)>', 'Follow-up',
    ]);
    await select.selectOption('1');
    await check(draft).toHaveValue('Draft for the other conversation');
    await screenshot(page, 'ide-chat-desktop.png');
    await page.setViewportSize({ width: 390, height: 664 });
    await check(page.locator('#new-chat')).toBeInViewport();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
    await screenshot(page, 'ide-chat-phone.png');
    await page.setViewportSize({ width: 1440, height: 1000 });
  } finally {
    release();
    await context.unroute(gateway, routeChat);
  }
  await page.locator('#open-workbench').click();
}

async function realJourney() {
  report.start('real.report.partial');
  await sourceSearchJourney();
  const fixture = await startFixture();
  report.setStage('context_start');
  report.ensureActive();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  report.setStage('scenario');
  const traffic = await observe(context);
  const { page, errors } = await connect(context, fixture);
  assert.equal(traffic.count('resume'), 1, 'startup resumes once');
  assert.equal(traffic.count('pair'), 1, 'one explicit pairing request');
  await chatJourney(page, context);
  await inspect(page, 'workspace');
  await check(page.locator('#workspace-panel .fg-pane-body textarea, #workbench [contenteditable="true"]')).toHaveCount(0);
  await check(page.locator('#workbench-chat')).toBeHidden();
  await check(page.getByRole('button', { name: /^(Run|Debug|Terminal|Save file)$/i })).toHaveCount(0);
  await check(page.locator('#source-shape-panel')).toHaveJSProperty('open', false);
  await check(page.locator('#inspection-details')).toHaveJSProperty('open', false);
  await check(page.locator('#problems-panel')).toBeHidden();
  await disclosure(page, 'inspection-details', true);
  await check(page.locator('#summary')).toContainText('1 traits');
  await check(page.locator('#summary')).toContainText('2 trait impls');
  await check(page.locator('#summary')).toContainText('1 free functions');
  await disclosure(page, 'inspection-details', false);
  const shapeCalls = traffic.count('inspect');
  await disclosure(page, 'source-shape-panel', true);
  await disclosure(page, 'source-shape-caveats', true);
  await disclosure(page, 'source-shape-counts', true);
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
  await disclosure(page, 'source-shape-caveats', false);
  await disclosure(page, 'source-shape-counts', false);
  await disclosure(page, 'source-shape-panel', false);
  assert.equal(traffic.count('inspect'), shapeCalls, 'shape navigation and disclosure are report-local');

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

  await inspect(page, 'empty', 'Project ready.');
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
  await inspect(page, 'workspace', 'Project ready.');
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
  await check(page.locator('#chat')).toBeVisible();
  await page.locator('#open-workbench').click();
  await page.locator('#project').selectOption('slow');
  await page.locator('#inspect').click();
  await check(page.locator('#phase-label')).toContainText('Inspecting');
  await page.locator('#project').focus();
  await check(page.locator('#status')).toContainText('Project ready.');
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

  await inspect(page, 'workspace', 'Project ready.');
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
  await page.locator('#open-workbench').click();
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
