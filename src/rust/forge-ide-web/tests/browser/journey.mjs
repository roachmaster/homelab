// Test-only browser driver. Not generated application JavaScript or a runtime asset.
// Every happy-path request uses the real generated worker/WASM -> host -> mTLS agent.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { readFile, realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { acquireOwned, bounded, cleanupResources, runHarness } from './report.mjs';
import { captureFailureScreenshot, screenshotEligibility, SCREENSHOT_MASK } from './diagnostics.mjs';
import { VIEWPORTS, verifyViewportProfiles } from './viewports.mjs';
import { requireRunningFixtureLeader } from './fixture-lifecycle.mjs';
import { mailSubject, startDurableRelay } from './durable-relay.mjs';

let assets, agent, fixtureBinary, output, browser, browserOwner, durableRelayOwner, check, report;
const active = new Set();
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function startFixture(faults = false, singleProject = false, { durable, managedWrites = false } = {}) {
  report.ensureActive();
  report.setStage('fixture_start');
  const mode = durable ? ['--durable', durable.bind, durable.endpoint, durable.tokenFile]
    : faults ? ['--faults'] : managedWrites ? ['--managed-writes'] : singleProject ? ['--single-project'] : [];
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
  const inspectionIds = new Set();
  context.on('request', request => {
    if (new URL(request.url()).pathname !== '/forge/v1/dispatch') return;
    let kind;
    try { kind = request.postDataJSON()?.kind; } catch { kind = 'invalid'; }
    if (!['resume', 'pair', 'inspect', 'source', 'chat_history_retention_set', 'chat_run_submit'].includes(kind)) kind = 'other';
    counts.set(kind, (counts.get(kind) || 0) + 1);
    if (kind === 'inspect') inspectionIds.add(request.headers()['forge-request-id']);
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
  return { count: kind => counts.get(kind) || 0, uniqueInspections: () => inspectionIds.size };
}
const beforeForgeUrl = 'data:text/html,<!doctype html><title>Forge QA previous page</title>';
const retainedChatDraft = 'Draft for the other conversation';

async function connect(context, fixture, autoLoad = false, beforeWorkbench) {
  report.ensureActive();
  const page = await context.newPage();
  fixture.page = page;
  const errors = [];
  let expectsLeaveWarning = false;
  page.on('pageerror', () => { if (errors.length < 64) errors.push('page runtime error'); });
  page.on('dialog', dialog => {
    if (expectsLeaveWarning && dialog.type() === 'beforeunload') return;
    if (errors.length < 64) errors.push('unexpected script dialog');
    void dialog.dismiss().catch(() => { if (errors.length < 64) errors.push('dialog dismissal failed'); });
  });
  async function respondToLeaveWarning(navigate, decision) {
    expectsLeaveWarning = true;
    try {
      const response = page.waitForEvent('dialog', { predicate: dialog => dialog.type() === 'beforeunload', timeout: 5000 })
        .then(dialog => decision === 'leave' ? dialog.accept() : dialog.dismiss());
      await Promise.all([response, navigate()]);
    } finally {
      expectsLeaveWarning = false;
    }
  }
  if (beforeWorkbench) await page.goto(beforeForgeUrl);
  await page.goto(fixture.origin, { waitUntil: 'domcontentloaded' });
  await check(page.locator('#pair')).toBeEnabled();
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#project option')).toHaveCount(0);
  await page.locator('#pair-code').fill(fixture.code);
  await page.locator('#pair').click();
  fixture.code = '';
  await check(page.locator('#chat')).toBeVisible();
  await check(page.locator('#workbench')).toBeHidden();
  await check(page.locator('#workbench-context')).toBeHidden();
  if (beforeWorkbench) await beforeWorkbench(page, respondToLeaveWarning);
  await page.locator('#open-workbench').click();
  await check(page.locator('#status')).toContainText(autoLoad ? 'Partial inspection:' : 'Connected.');
  await check(page.locator('#pair-code')).toHaveValue('');
  await check(page.locator('#pair-panel')).toBeHidden();
  await check(page.locator('#node')).toHaveValue('lab');
  await check(page.locator('#workbench-context')).toHaveText('Local fixture / workspace');
  await check(page.locator('#workbench-context')).toBeVisible();
  await check(page.locator('#project-options')).toHaveJSProperty('open', false);
  await check(page.locator('#node')).toBeHidden();
  await check(page.locator('#project')).toBeHidden();
  assert.equal(page.workers().length, 1, 'one real dedicated WASM worker');
  return { page, errors, respondToLeaveWarning };
}
function checkpointSaved(page, matches) {
  return page.waitForResponse(response => {
    if (!response.ok() || new URL(response.url()).pathname !== '/forge/v1/dispatch/workspace') return false;
    const command = response.request().postDataJSON();
    return command?.operation === 'save' && matches(command.snapshot);
  }, { timeout: 10000 });
}
function checkpointHasDraft(snapshot, draft) {
  return snapshot?.chat?.conversations?.some(conversation => conversation.draft === draft) === true;
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
async function screenshot(page, name, { home = false } = {}) {
  assert.equal(await screenshotEligibility(page, [...active], { home }), 'safe', 'Only sanitized owned fixture pages may be captured');
  if (!home) await check(page.locator('#pair-code')).toHaveValue('');
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
async function selectProject(page, project) {
  await disclosure(page, 'project-options', true);
  await page.locator('#project').selectOption(project);
}
async function inspect(page, project, expected = 'Partial inspection:') {
  await selectProject(page, project);
  await disclosure(page, 'project-options', false);
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
  const caller = Number(new Error().stack.split('\n').slice(2)
    .map(line => line.match(/journey\.mjs:(\d+):\d+/)?.[1]).find(Boolean) ?? 0);
  let stage = 'read';
  try {
    if (await details.evaluate(node => node.open) !== open) {
      const summary = page.locator(`#${id} > summary`);
      stage = 'focus';
      await summary.focus();
      await check(summary).toBeFocused();
      stage = 'toggle';
      await page.keyboard.press('Enter');
      await check(summary).toBeFocused();
    }
    stage = 'state';
    await check(details).toHaveJSProperty('open', open);
  } catch (error) {
    const allowed = ['chat-options', 'file-metadata-panel', 'inspection-details', 'project-options',
      'source-shape-caveats', 'source-shape-counts', 'source-shape-panel', 'workspace-options'];
    const sample = await page.evaluate(ids => {
      const active = document.activeElement;
      const summary = active?.matches('summary') ? active.parentElement?.id : null;
      const controls = ['chat-draft', 'chat-model', 'send-chat', 'open-chat', 'open-mail',
        'open-workbench', 'chat-history-toggle', 'chat-conversation', 'chat-run-cancel', 'chat-run-retry'];
      return active === document.body ? 'body' : ids.includes(summary) ? summary + '-summary'
        : controls.includes(active?.id) ? active.id : 'other';
    }, allowed).catch(() => 'unavailable');
    process.stderr.write('QA_CHECK disclosure_failure=' + JSON.stringify({
      disclosure: allowed.includes(id) ? id : 'other', stage, active: sample,
      caller: Number.isInteger(caller) && caller >= 0 && caller <= 100000 ? caller : 0,
    }) + '\n');
    throw error;
  }
}
async function openChatHistory(page) {
  const drawer = page.locator('#chat-history-panel');
  if (!await drawer.evaluate(node => node.open)) {
    await page.locator('#chat-history-toggle').click();
  }
  await check(drawer).toBeVisible();
  await check(drawer).toHaveJSProperty('open', true);
  return drawer;
}
async function selectChat(page, value) {
  await openChatHistory(page);
  await page.locator('#chat-conversation').selectOption(value);
  await check(page.locator('#chat-history-panel')).toBeHidden();
}
async function openChatOptions(page) {
  await check(page.locator('#chat')).toBeVisible();
  await check(page.locator('#chat-options > summary')).toBeVisible();
  await disclosure(page, 'chat-options', true);
}

async function checkRefreshPlacement(page) {
  const problems = page.locator('#problems-panel-switch');
  const refresh = page.getByRole('button', { name: 'Refresh project', exact: true });
  await check(refresh).toHaveAttribute('id', 'inspect');
  await check(refresh).toHaveAttribute('data-action', 'inspect');
  await check(refresh).toHaveAttribute('aria-label', 'Refresh project');
  await check(refresh).toHaveAttribute('title', 'Refresh project');
  await check(refresh).toHaveText('');
  await check(refresh.locator('[aria-hidden="true"][data-glyph="↻"]')).toBeVisible();
  await check(refresh).toBeEnabled();
  await check(problems).toBeInViewport();
  await check(refresh).toBeInViewport();
  assert.ok(await refresh.evaluate(button => {
    const reference = document.createElement('span');
    reference.style.backgroundColor = 'var(--raised)';
    document.body.append(reference);
    const neutral = getComputedStyle(button).backgroundColor === getComputedStyle(reference).backgroundColor;
    reference.remove();
    return neutral;
  }), 'refresh uses the neutral raised surface');
  const [problemsBounds, refreshBounds] = await Promise.all([problems.boundingBox(), refresh.boundingBox()]);
  assert(problemsBounds && refreshBounds && problemsBounds.y < refreshBounds.y + refreshBounds.height &&
    refreshBounds.y < problemsBounds.y + problemsBounds.height, 'Problems and Refresh project share a row');
  if (page.viewportSize().width <= 390) {
    assert(Math.abs(refreshBounds.width - 44) < 1 && Math.abs(refreshBounds.height - 44) < 1,
      'phone refresh target is 44 by 44 CSS pixels');
  }
  const gap = refreshBounds.x - (problemsBounds.x + problemsBounds.width);
  assert(gap >= -1 && gap <= 12, 'Refresh project immediately follows Problems');
  await problems.focus();
  await page.keyboard.press('Tab');
  await check(refresh).toBeFocused();
  await refresh.click({ trial: true });
}
async function projectOptionsJourney(page, traffic) {
  await disclosure(page, 'workspace-options', false);
  await disclosure(page, 'project-options', false);
  await checkRefreshPlacement(page);
  const projectDetails = page.locator('#project-options');
  const viewDetails = page.locator('#workspace-options');
  const projectOptions = page.locator('#project-options > summary');
  const viewOptions = page.locator('#workspace-options > summary');
  const projectPanel = page.locator('#project-options > .fg-disclosure-body');
  const viewPanel = page.locator('#workspace-options > .fg-option-list');
  await check(projectOptions).toHaveText('Project options');
  await check(projectOptions).toBeInViewport();
  await check(viewOptions).toBeInViewport();
  const projectBounds = await projectOptions.boundingBox();
  const viewBounds = await viewOptions.boundingBox();
  assert(projectBounds && viewBounds && projectBounds.y < viewBounds.y + viewBounds.height &&
    viewBounds.y < projectBounds.y + projectBounds.height, 'project and view options share a row');
  const requests = ['resume', 'pair', 'inspect', 'source', 'other'];
  const before = requests.map(traffic.count);
  await disclosure(page, 'workspace-options', true);
  await check(viewPanel).toBeVisible();
  await check(projectDetails).toHaveJSProperty('open', false);
  await check(projectPanel).toBeHidden();
  await disclosure(page, 'project-options', true);
  await check(projectPanel).toBeVisible();
  await check(viewDetails).toHaveJSProperty('open', false);
  await check(viewPanel).toBeHidden();
  await page.keyboard.press('Tab');
  await check(page.locator('#node')).toBeFocused();
  await page.keyboard.press('Tab');
  await check(page.locator('#project')).toBeFocused();
  assert.ok(await page.locator('#project-options select').evaluateAll(fields =>
    fields.length === 2 && fields.every(field => field.getBoundingClientRect().width >= 120)),
  'open context selectors keep readable widths');
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
    'open project options do not overflow the viewport');
  await disclosure(page, 'workspace-options', true);
  await check(viewPanel).toBeVisible();
  await check(projectDetails).toHaveJSProperty('open', false);
  await check(projectPanel).toBeHidden();
  await check(page.locator('#node')).toBeHidden();
  await check(page.locator('#project')).toBeHidden();
  await disclosure(page, 'workspace-options', false);
  await check(viewPanel).toBeHidden();
  assert.deepEqual(requests.map(traffic.count), before, 'view and project options open and close locally');
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
  await check(page.locator('#source-view')).toContainText('pub fn broken(');
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
  await check(page.locator('#inspection-actions')).toBeHidden();
  await check(page.locator('#cancel')).toBeHidden();
  await check(page.locator('#phase-label')).toBeHidden();
  await page.locator('#problems-panel-switch').focus();
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
    if (viewport.width === 375) await projectOptionsJourney(page, traffic);
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

async function reportAndPageLifecycle(page, traffic, respondToLeaveWarning) {
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
  const saved = checkpointSaved(page, snapshot => snapshot.workbench_open &&
    checkpointHasDraft(snapshot, retainedChatDraft) && snapshot.workbench?.entry?.path.at(-1)?.value === 'README <safe>.md');
  for (let i = 0; i < 3; i += 1) {
    await target.click();
    await check(target).toBeFocused();
    await check(page.locator('#files [role="treeitem"]')).toHaveCount(rows);
  }
  await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
  await saved;
  report.pass('probe.report.stale_actions');
  report.start('probe.page.lifecycle');

  await page.locator('#source-display').click();
  await check(page.locator('#source-preview')).toContainText('Fixture overview');
  const requestCounts = () => ['resume', 'inspect', 'source'].map(traffic.count);
  async function checkRestoredWorkbench(before) {
    await check(page.locator('#workbench')).toBeVisible();
    await check(page.locator('#selection-title')).toContainText('README <safe>.md');
    await check(page.locator('#source-view')).toBeVisible();
    await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
    await check(page.locator('#source-raw')).toHaveAttribute('aria-pressed', 'true');
    assert.deepEqual(requestCounts(), before.map(count => count + 1),
      'workspace restore authenticates, inspects, and verifies source afresh');
    assert.equal(page.workers().length, 1);
  }
  let before = requestCounts();
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
      contextHidden: document.getElementById('workbench-context').hidden,
      contextText: document.getElementById('workbench-context').textContent,
      threadTitleCleared: document.getElementById('chat-history-toggle').textContent === 'Previous chats',
      source: document.getElementById('source-view').textContent,
      markdown: document.getElementById('source-preview').textContent,
      transcript: document.getElementById('chat-transcript').textContent,
      draft: document.getElementById('chat-draft').value,
    };
  });
  assert.equal(cleared.options, 0, 'teardown synchronously clears protected catalog DOM');
  assert.equal(cleared.titlesCleared, true, 'teardown also removes catalog metadata from select titles');
  assert.equal(cleared.rows, 0, 'teardown synchronously clears report rows');
  assert.equal(cleared.details, '', 'teardown synchronously clears metadata');
  assert.equal(cleared.declarations, '', 'teardown synchronously clears declaration metadata');
  assert.equal(cleared.disclosuresClosed, true, 'teardown closes disclosures as well as clearing their protected contents');
  assert.equal(cleared.hidden, true, 'teardown hides protected composition before resume');
  assert.equal(cleared.contextHidden, true, 'teardown hides project context before resume');
  assert.equal(cleared.contextText, '', 'teardown clears project context before resume');
  assert.equal(cleared.threadTitleCleared, true, 'teardown clears the protected active thread title before resume');
  assert.equal(cleared.source, '', 'teardown clears verified source before resume');
  assert.equal(cleared.markdown, '', 'teardown clears displayed Markdown before resume');
  assert.equal(cleared.transcript, '', 'teardown clears the transcript before resume');
  assert.equal(cleared.draft, '', 'teardown clears the draft before resume');
  await page.evaluate(() => {
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
  });
  await checkRestoredWorkbench(before);
  process.stderr.write('QA_CHECK native_back_warning\n');
  await page.locator('#workbench-context').click();
  before = requestCounts();
  const forgeUrl = page.url();
  await respondToLeaveWarning(() => page.evaluate(() => history.back()), 'stay');
  await check(page).toHaveURL(forgeUrl);
  await check(page.locator('#selection-title')).toContainText('README <safe>.md');
  assert.deepEqual(requestCounts(), before, 'dismissing Back keeps the current session and report');
  await respondToLeaveWarning(() => page.evaluate(() => history.back()), 'leave');
  await check(page).toHaveURL(beforeForgeUrl);
  process.stderr.write('QA_CHECK forward_restore\n');
  await page.goForward({ waitUntil: 'domcontentloaded' });
  await checkRestoredWorkbench(before);
  process.stderr.write('QA_CHECK reload_restore\n');
  await page.locator('#workbench-context').click();
  before = requestCounts();
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await checkRestoredWorkbench(before);
  await page.locator('#open-chat').click();
  await check(page.locator('#chat')).toBeVisible();
  const conversations = page.locator('#chat-conversation');
  await check(conversations).toHaveValue('1');
  await check(page.locator('#chat-draft')).toHaveValue(retainedChatDraft);
  await selectChat(page, '0');
  await check(page.locator('#chat-transcript')).toContainText('Original conversation');
  await check(page.locator('#chat-transcript [data-role="assistant"]')).toHaveCount(2);
  await page.locator('#open-workbench').click();
  assert.equal(await page.evaluate(() => localStorage.length === 0 && sessionStorage.length === 1 &&
    /^[0-9a-f]{32}$/.test(sessionStorage.getItem('forge-workspace-tab') ?? '')), true,
  'browser storage contains only the opaque same-tab checkpoint identifier');
  assert.equal(new URL(page.url()).search, '', 'no report/session state in URL');
  const otherTab = await page.context().newPage();
  try {
    const portalUrl = page.url();
    const staleUrl = new URL('/obsolete-workbench', portalUrl).href;
    // Seed one genuine history entry for a page that used to exist. Only this
    // visit is simulated. Back below reaches the native host document route.
    await otherTab.route(staleUrl, route => route.fulfill({
      status: 200,
      contentType: 'text/html',
      headers: { 'Cache-Control': 'no-store' },
      body: '<!doctype html><title>Prior Forge page</title>',
    }), { times: 1 });
    await otherTab.goto(staleUrl, { waitUntil: 'domcontentloaded' });
    await otherTab.goto(portalUrl, { waitUntil: 'domcontentloaded' });
    await check(otherTab.locator('#chat')).toBeVisible();
    const draft = otherTab.locator('#chat-draft');
    await check(draft).toHaveValue('');
    await check(otherTab.locator('#chat-conversation option')).toHaveCount(1);
    await noReport(otherTab);

    const staleHistoryDraft = 'Keep this draft when Back reaches an old Forge page';
    const saved = checkpointSaved(otherTab, snapshot => checkpointHasDraft(snapshot, staleHistoryDraft));
    await draft.click();
    await draft.fill(staleHistoryDraft);
    await saved;
    process.stderr.write('QA_CHECK stale_portal_back\n');
    const documentResponse = otherTab.waitForResponse(response =>
      response.url() === staleUrl && response.request().isNavigationRequest());
    const leave = otherTab.waitForEvent('dialog', { predicate: dialog => dialog.type() === 'beforeunload', timeout: 5000 })
      .then(dialog => dialog.accept());
    const [redirect] = await Promise.all([
      documentResponse,
      leave,
      otherTab.waitForEvent('domcontentloaded'),
      otherTab.evaluate(() => history.back()),
    ]);
    assert.match(await redirect.request().headerValue('accept') ?? '', /\btext\/html\b/);
    assert.equal(redirect.status(), 302);
    assert.equal(redirect.headers().location, '/');
    await check(otherTab).toHaveURL(portalUrl);
    await check(otherTab.locator('#chat')).toBeVisible();
    await check(draft).toHaveValue(staleHistoryDraft);
  } finally {
    await otherTab.close();
  }
  await durableHostRestartJourney();
  report.pass('probe.page.lifecycle');
}

async function mailLayoutJourney(page, desktop) {
  process.stderr.write('QA_CHECK mail_folder_layout\n');
  const inbox = page.locator('#mail-inbox');
  const archive = page.locator('#mail-archive');
  const refresh = page.getByRole('button', { name: 'Refresh Mail', exact: true });
  const row = page.locator('#mail-threads [role="treeitem"]').filter({ hasText: mailSubject });
  await page.setViewportSize({ width: 320, height: 720 });
  await page.locator('#open-mail').click();
  await check(inbox).toHaveAttribute('aria-current', 'page');
  await check(inbox).toHaveCSS('border-top-color', await themeColor(page, 'accent'));
  await check(archive).not.toHaveAttribute('aria-current', 'page');
  await check(row.locator('.fg-row-label')).toHaveText(mailSubject);
  await check(row.locator('.fg-row-secondary')).toHaveText('Reply ready · Nov 16, 2026 · 22:11 UTC');
  await check(row.locator('.fg-glyph')).toBeHidden();

  for (const width of [320, 375, 390]) {
    await page.setViewportSize({ width, height: 720 });
    const [bounds, subject, details, icon, folders, compose] = await Promise.all([
      row.boundingBox(), row.locator('.fg-row-label').boundingBox(),
      row.locator('.fg-row-secondary').boundingBox(), refresh.boundingBox(),
      page.locator('#mail-folders').boundingBox(), page.locator('#mail-new').boundingBox(),
    ]);
    assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= width + 1, 'Mail row fits the phone');
    assert.ok(subject && details && details.y >= subject.y + subject.height, 'metadata sits below its subject');
    assert.ok(icon && icon.width >= 44 && icon.height >= 44 && Math.abs(icon.width - icon.height) <= 1,
      'Refresh has a compact square touch target');
    assert.ok(folders && icon && compose && Math.abs(folders.y - compose.y) <= 1 && Math.abs(icon.y - compose.y) <= 1,
      'Mail folders and actions fit on a single phone toolbar row');
    assert.equal(await row.evaluate(node => [...node.children].filter(part => part.getClientRects().length)
      .every(part => part.scrollWidth <= part.clientWidth + 1)), true, 'the subject and metadata wrap without clipping');
    assert.equal(await page.evaluate(() => {
      window.scrollTo(1000, 0);
      return document.documentElement.scrollWidth <= innerWidth + 1 && window.scrollX === 0;
    }), true, 'the phone page cannot scroll sideways');
  }
  await archive.click();
  await check(archive).toHaveAttribute('aria-current', 'page');
  await check(inbox).not.toHaveAttribute('aria-current', 'page');
  const archivedRow = page.locator('#mail-threads [role="treeitem"]').filter({ hasText: 'Archived fixture thread' });
  await check(archivedRow.locator('.fg-row-secondary')).toHaveText('Archived · Read only · Nov 17, 2026 · 01:23 UTC');
  await inbox.click();
  await check(row).toBeVisible();

  await page.setViewportSize(desktop);
  await row.click();
  await check(page.locator('#mail-title')).toHaveText(mailSubject);
  const [sidebar, reader] = await Promise.all([
    page.locator('#mail-list-panel').boundingBox(), page.locator('#mail-reader').boundingBox(),
  ]);
  assert.ok(sidebar && reader && sidebar.x + sidebar.width <= reader.x, 'desktop keeps the thread list beside the reader');
  await page.setViewportSize({ width: 320, height: 720 });
  await check(page.locator('#mail-list-panel')).toBeHidden();
  await check(page.locator('#mail-reader')).toBeVisible();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
    'the single-pane reader fits the narrow phone');
  await page.setViewportSize(desktop);
  await page.locator('#open-chat').click();
}

async function durableHostRestartJourney() {
  const prompt = 'Keep this submitted conversation';
  const reply = '## Durable fixture reply\n\n**Ready after restart.**';
  const privateDraft = 'Keep this unsent text in its original tab';
  const cancelledDraft = 'Keep this message when the queued run is cancelled';
  const file = 'README <safe>.md';
  const source = 'Fixture <script> text is never HTML.';
  const viewport = { width: 1440, height: 1000 };
  const contexts = [];
  let fixture;
  durableRelayOwner = acquireOwned(() => startDurableRelay(prompt, reply), relay => relay.close());
  try {
    const relay = await durableRelayOwner.ready;
    async function restart() {
      const previous = fixture;
      if (previous) await stopFixture(previous);
      fixture = await startFixture(false, false, { durable: {
        bind: previous ? new URL(previous.origin).host : '127.0.0.1:0',
        endpoint: relay.endpoint, tokenFile: relay.tokenFile,
      } });
      report.setStage('scenario');
      if (previous) assert.equal(fixture.origin, previous.origin, 'new host owns the same browser origin');
    }
    async function startPage() {
      const context = await browser.newContext({ viewport });
      contexts.push(context);
      const traffic = await observe(context);
      const page = await context.newPage();
      fixture.page = page;
      await page.goto(fixture.origin, { waitUntil: 'domcontentloaded' });
      return { page, traffic };
    }
    async function pair(page) {
      await check(page.locator('#pair')).toBeEnabled();
      await check(page.locator('#chat')).toBeHidden();
      await check(page.locator('#chat-transcript [data-role]')).toHaveCount(0);
      await check(page.locator('#chat-draft')).toHaveValue('');
      await check(page.locator('#workbench')).toBeHidden();
      await page.locator('#pair-code').fill(fixture.code);
      await page.locator('#pair').click();
      fixture.code = '';
      await check(page.locator('#pair-panel')).toBeHidden();
    }
    function tabIdentifier(page) {
      return page.evaluate(() => sessionStorage.getItem('forge-workspace-tab'));
    }
    async function reloadQueuedChat(page) {
      await page.locator('#chat-status').click();
      const leave = page.waitForEvent('dialog', { predicate: dialog => dialog.type() === 'beforeunload', timeout: 5000 })
        .then(dialog => dialog.accept());
      await Promise.all([leave, page.reload({ waitUntil: 'domcontentloaded' })]);
      await openChatOptions(page);
      await check(page.locator('#chat-run-cancel')).toBeVisible();
      await check(page.locator('#chat-status')).toHaveText('Waiting for Forge…');
    }
    async function checkTranscript(page) {
      const transcript = page.locator('#chat-transcript');
      await check(transcript.locator('[data-role="user"]')).toHaveCount(1);
      await check(transcript.locator('[data-role="user"]')).toContainText(prompt);
      await check(transcript.locator('[data-role="assistant"]')).toHaveCount(1);
      await check(transcript.getByRole('heading', { name: 'Durable fixture reply' })).toBeVisible();
      await check(transcript.locator('strong').filter({ hasText: /^Ready after restart\.$/ })).toBeVisible();
    }

    process.stderr.write('QA_CHECK durable_chat_save\n');
    await restart();
    const { page, traffic } = await startPage();
    await pair(page);
    await mailLayoutJourney(page, viewport);
    await check(page.locator('#chat')).toBeVisible();
    const model = page.locator('#chat-model');
    await openChatOptions(page);
    await check(model).toHaveValue('gpt-oss-20b');
    await check(model.locator('option[value="qwen2.5-coder-7b"]')).toHaveText(/Qwen2.5-Coder 7B.*loads on request$/);
    await model.selectOption('qwen2.5-coder-7b');
    const tab = await tabIdentifier(page);
    assert.ok(/^[0-9a-f]{32}$/.test(tab ?? ''), 'first document uses an opaque tab key');
    const draft = page.locator('#chat-draft');
    await draft.fill(prompt);
    await page.locator('#send-chat').click();
    await check(page.locator('#chat-run-retry')).toBeVisible();
    assert.equal(relay.submitted(), 1, 'the queue committed the job before the acknowledgement was lost');
    const accepted = checkpointSaved(page, snapshot => !!snapshot.chat.pending_run?.run_id &&
      snapshot.chat.pending_run.model_id === 'qwen2.5-coder-7b');
    await reloadQueuedChat(page);
    await accepted;
    await check(model).toHaveValue('qwen2.5-coder-7b');
    await check(model).toBeDisabled();
    assert.equal(relay.submitted(), 1, 'reloading with no run ID reuses the submitted intent');
    await reloadQueuedChat(page);
    assert.equal(traffic.count('chat_run_submit'), 2, 'reloading with a run ID polls without submitting');
    relay.finish();
    await checkTranscript(page);
    await check(model).toBeEnabled();
    await check(model.locator('option[value="qwen2.5-coder-7b"]')).toHaveText(/ · loaded$/);
    await page.locator('#new-chat').click();
    await draft.fill(privateDraft);
    const persisted = checkpointSaved(page, snapshot => snapshot.workbench_open &&
      snapshot.workbench?.entry?.path.at(-1)?.value === file && checkpointHasDraft(snapshot, privateDraft) &&
      snapshot.chat.conversations[0]?.messages.some(message => message.role === 'assistant' && message.content === reply));
    await page.locator('#open-workbench').click();
    await inspect(page, 'workspace');
    await fileRow(page, file).click();
    await check(page.locator('#source-view')).toContainText(source);
    await persisted;

    process.stderr.write('QA_CHECK durable_chat_same_tab_restart\n');
    const inspectCalls = traffic.count('inspect');
    const sourceCalls = traffic.count('source');
    await restart();
    fixture.page = page;
    const leave = page.waitForEvent('dialog', { predicate: dialog => dialog.type() === 'beforeunload', timeout: 5000 })
      .then(dialog => dialog.accept());
    await Promise.all([leave, page.reload({ waitUntil: 'domcontentloaded' })]);
    await pair(page);
    await check(page.locator('#workbench')).toBeVisible();
    await check(page.locator('#selection-title')).toContainText(file);
    await check(page.locator('#source-view')).toContainText(source);
    assert.equal(traffic.count('inspect'), inspectCalls + 1, 'new host performs a fresh authenticated inspection');
    assert.equal(traffic.count('source'), sourceCalls + 1, 'new host authorizes the source from the fresh report');
    assert.ok(await tabIdentifier(page) === tab, 'reload retains the opaque tab key');
    await page.locator('#open-chat').click();
    await check(page.locator('#chat-conversation')).toHaveValue('1');
    await check(draft).toHaveValue(privateDraft);
    await selectChat(page, '0');
    await checkTranscript(page);

    process.stderr.write('QA_CHECK durable_chat_new_context\n');
    await restart();
    const fresh = await startPage();
    await pair(fresh.page);
    await check(fresh.page.locator('#chat')).toBeVisible();
    await checkTranscript(fresh.page);
    await check(fresh.page.locator('#chat-draft')).toHaveValue('');
    await check(fresh.page.locator('#chat-conversation option')).toHaveCount(1);
    await check(fresh.page.locator('#workbench')).toBeHidden();
    await noReport(fresh.page);
    assert.ok(await tabIdentifier(fresh.page) !== tab, 'independent browser context gets a new tab key');
    assert.equal(fresh.traffic.count('inspect'), 0, 'the previous tab Workbench bookmark does not transfer');
    assert.equal(fresh.traffic.count('source'), 0);
    assert.equal(relay.completions(), 1, 'reopening the transcript does not replay inference');

    process.stderr.write('QA_CHECK durable_chat_history_retention_and_delete\n');
    const freshPage = fresh.page;
    await freshPage.setViewportSize({ width: 390, height: 664 });
    const previousChats = freshPage.getByRole('button', { name: 'Previous chats' });
    const manage = freshPage.getByRole('button', { name: 'Manage current chat', exact: true });
    const savedChats = freshPage.getByRole('tree', { name: 'Saved chats' });
    await previousChats.click();
    const savedChat = savedChats.getByRole('treeitem', { name: prompt });
    await check(savedChat).toBeVisible();
    await check(savedChat).toContainText('Kept through 2026-12-16 (UTC)');
    const search = freshPage.getByRole('searchbox', { name: 'Search saved chats on this page', exact: true });
    const beforeSearch = fresh.traffic.count('other');
    await search.fill('No matching saved conversation');
    await check(savedChats.getByRole('treeitem')).toHaveCount(0);
    await search.fill('KEEP THIS');
    await check(savedChat).toBeVisible();
    assert.equal(fresh.traffic.count('other'), beforeSearch, 'filtering the current history page stays local to WASM');
    await search.fill('');
    await screenshot(freshPage, 'ide-chat-history-phone.png');
    await savedChat.click();
    await check(previousChats).toHaveAttribute('aria-expanded', 'false');
    await checkTranscript(freshPage);
    await openChatHistory(freshPage);
    await manage.click();
    await check(freshPage.locator('#chat-retention-label')).toHaveText('Kept through 2026-12-16 (UTC)');
    assert.equal(fresh.traffic.count('chat_history_retention_set'), 0, 'the default date needs no browser mutation');
    await freshPage.getByRole('button', { name: 'Set expiration', exact: true }).click();
    const expiration = freshPage.getByRole('dialog', { name: 'Set expiration' });
    await expiration.getByLabel('Keep through').fill('2099-05-12');
    await expiration.getByRole('button', { name: 'Save expiration' }).click();
    await check(expiration).toBeHidden();
    await check(freshPage.locator('#chat-retention-label')).toHaveText('Kept through 2099-05-12 (UTC)');
    await freshPage.getByRole('button', { name: 'Keep long term', exact: true }).click();
    await check(freshPage.locator('#chat-retention-label')).toHaveText('Kept long term');
    assert.equal(fresh.traffic.count('chat_history_retention_set'), 2, 'only explicit changes update retention');
    const deleteButton = freshPage.locator('#chat-show-delete');
    const deletion = freshPage.getByRole('dialog', { name: 'Delete chat' });
    await deleteButton.click();
    await check(deletion).toContainText(prompt);
    await check(deletion.getByRole('button', { name: 'Cancel' })).toBeFocused();
    await deletion.getByRole('button', { name: 'Cancel' }).click();
    await check(deletion).toBeHidden();
    await checkTranscript(freshPage);
    await check(freshPage.locator('#chat-history-panel')).toBeVisible();
    await deleteButton.click();
    await deletion.getByRole('button', { name: 'Delete chat', exact: true }).click();
    await check(deletion).toBeHidden();
    await check(freshPage.locator('#chat-history-status')).toHaveText('Chat deleted.');
    await check(freshPage.locator('#chat-history-panel')).toBeHidden();
    await openChatHistory(freshPage);
    await check(savedChats).toHaveText('No saved chats yet.');
    await check(freshPage.locator('#chat-transcript [data-role]')).toHaveCount(0);
    assert.equal(await freshPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true);
    assert.equal(relay.completions(), 1, 'retention and deletion do not call inference');
    await freshPage.locator('#chat-history-close').click();
    await check(freshPage.locator('#chat-history-panel')).toBeHidden();
    await freshPage.locator('#new-chat').click();
    await check(freshPage.locator('#chat-conversation')).toHaveValue('1');
    const cancelled = freshPage.locator('#chat-draft');
    await cancelled.fill(cancelledDraft);
    await freshPage.locator('#send-chat').click();
    await openChatOptions(freshPage);
    await check(freshPage.locator('#chat-run-cancel')).toBeVisible();
    await freshPage.locator('#chat-run-cancel').click();
    await check(freshPage.locator('#chat-status')).toContainText('Run cancelled');
    await check(cancelled).toHaveValue(cancelledDraft);
    await check(freshPage.locator('#chat-transcript [data-role]')).toHaveCount(0);

  } finally {
    try { await Promise.all(contexts.map(context => context.close())); }
    finally {
      try { if (fixture) await stopFixture(fixture); }
      finally { await durableRelayOwner.close(); }
    }
  }
}
async function noSelectedFileFromPriorReport(page) {
  await check(page.locator('#selection-title')).not.toContainText(LONG_FILE_NAME);
  await check(page.locator('#details')).not.toContainText(LONG_FILE_NAME);
}

async function checkPhoneFileBack(page) {
  const header = page.locator('#workspace-panel > .fg-pane-header');
  const heading = header.getByRole('heading', { name: 'File', exact: true });
  const back = header.getByRole('button', { name: 'Back to files', exact: true });
  await check(back).toHaveAttribute('id', 'back-to-files');
  await check(back).toHaveAttribute('aria-label', 'Back to files');
  await check(back).toHaveAttribute('title', 'Back to files');
  await check(back).not.toContainText('Back to files');
  await check(back).toBeInViewport();
  const [icon, title] = await Promise.all([back.boundingBox(), heading.boundingBox()]);
  assert(icon && title && Math.abs(icon.width - 44) < 1 && Math.abs(icon.height - 44) < 1,
    'the phone Back icon has a 44 by 44 CSS pixel target');
  assert(icon.x + icon.width <= title.x + 1 && icon.y < title.y + title.height && title.y < icon.y + icon.height,
    'the Back icon and File heading share the same header row');
  await back.focus();
  await page.keyboard.press('Enter');
  await check(page.locator('#explorer-panel')).toBeVisible();
  await check(page.locator('#workspace-panel')).toBeHidden();
  await assertVisibleFocus(page);
}

async function checkMarkdownSource(page, traffic) {
  const modes = page.getByRole('group', { name: 'Markdown view', exact: true });
  const raw = modes.getByRole('button', { name: 'Raw', exact: true });
  const display = modes.getByRole('button', { name: 'Display', exact: true });
  const rawView = page.locator('#source-view');
  const rawLines = rawView.locator('.fg-source-text');
  const preview = page.getByRole('region', { name: 'Markdown display', exact: true });
  const lines = [
    '## Fixture overview', '', '**Readable detail**', '', 'Fixture <script> text is never HTML.', '',
    '![Fixture image alternative](/fixture-diagram.png)',
  ];
  async function checkRaw() {
    await check(raw).toHaveAttribute('aria-pressed', 'true');
    await check(display).toHaveAttribute('aria-pressed', 'false');
    await check(rawView).toBeVisible();
    await check(preview).toBeHidden();
    await check(rawLines).toHaveCount(lines.length);
    assert.deepEqual(await rawLines.allTextContents(), lines, 'Raw shows each accepted source line verbatim');
  }
  await check(modes).toBeVisible();
  await checkRaw();
  const counts = ['source', 'inspect'].map(traffic.count);
  await display.click();
  await check(display).toHaveAttribute('aria-pressed', 'true');
  await check(raw).toHaveAttribute('aria-pressed', 'false');
  await check(rawView).toBeHidden();
  await check(preview.getByRole('heading', { name: 'Fixture overview', exact: true })).toBeVisible();
  await check(preview.locator('strong')).toHaveText('Readable detail');
  await check(preview).toContainText('Fixture <script> text is never HTML.');
  await check(preview).toContainText('Fixture image alternative');
  await check(preview.locator('script,img,svg')).toHaveCount(0);
  await check(preview).not.toContainText('## Fixture overview');
  await check(preview).not.toContainText('**Readable detail**');
  await raw.press('Enter');
  await checkRaw();
  assert.deepEqual(['source', 'inspect'].map(traffic.count), counts, 'changing Markdown view reuses verified source');
}

// Actual mTLS source reads, including the single-project phone entry path.
// Search is report-local; opening a result obtains its digest-checked source.
async function sourceSearchJourney() {
  const managedWrites = process.platform !== 'win32' && process.getuid?.() !== 0;
  const fixture = await startFixture(false, true, { managedWrites });
  report.setStage('scenario');
  const context = await browser.newContext({ viewport: { width: 390, height: 664 } });
  try {
    const traffic = await observe(context);
    const { page, errors, respondToLeaveWarning } = await connect(context, fixture, true);
    await projectOptionsJourney(page, traffic);
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
    await check(page.locator('#source-format')).toBeHidden();
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
      await checkPhoneFileBack(page);
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
    await check(page.locator('#workspace-panel')).toBeVisible();
    await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
    await check(page.locator('#source-view script, #source-view img, #source-view svg')).toHaveCount(0);
    await checkMarkdownSource(page, traffic);
    await page.getByRole('button', { name: 'Back to files', exact: true }).click();
    await check(page.locator('#explorer-panel')).toBeVisible();
    await page.locator('#project-query').fill('paged.txt');
    await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'paged.txt' }).click();
    await check(page.locator('#source-view')).toContainText('Source line 1:');
    await check(page.locator('#source-format')).toBeHidden();
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
    const saved = checkpointSaved(page, snapshot => snapshot.workbench_open &&
      snapshot.workbench?.entry?.declaration.at(-1)?.name === 'late_view');
    await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'late_view' }).click();
    const lateLine = page.locator('#source-view [data-line="300"]');
    await check(lateLine).toHaveAttribute('data-highlight', 'true');
    await check.poll(() => lateLine.evaluate(line => {
      const source = line.closest('[data-source-view]');
      return Math.abs(line.getBoundingClientRect().top - source.getBoundingClientRect().top - source.clientTop);
    })).toBeLessThan(3);
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    await check(page.locator('#source-view [data-line="301"]')).toBeInViewport();
    await saved;
    const beforeReload = traffic.count('inspect');
    const sourceBeforeReload = traffic.count('source');
    await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
    await check(page.locator('#workbench')).toBeVisible();
    await check(page.locator('#status')).toContainText('Partial inspection:');
    await check(lateLine).toHaveAttribute('data-highlight', 'true');
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    assert.equal(traffic.count('inspect'), beforeReload + 1, 'reload obtains a fresh single-project report');
    assert.equal(traffic.count('source'), sourceBeforeReload + 1, 'reload re-verifies the bookmarked declaration source');
    const designDraft = 'Keep this draft while I read the Forge design';
    await page.locator('#open-chat').click();
    await page.locator('#chat-draft').fill(designDraft);
    await page.locator('#open-workbench').click();
    await homeJourney(page, traffic);
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    const beforeDesign = ['resume', 'inspect', 'source'].map(traffic.count);
    await designJourney(page, traffic);
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    assert.deepEqual(['resume', 'inspect', 'source'].map(traffic.count), beforeDesign.map(count => count + 1),
      'returning from Design authenticates and re-verifies the bookmarked source');
    await page.locator('#open-chat').click();
    await check(page.locator('#chat-draft')).toHaveValue(designDraft);
    if (managedWrites) {
      await page.setViewportSize({ width: 390, height: 844 });
      await page.locator('#open-workbench').click();
      await managedWriteJourney(page, context, respondToLeaveWarning);
      await page.locator('#open-chat').click();
    }
    await designLocalRestoreJourney(page);
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

async function managedWriteJourney(page, context, respondToLeaveWarning) {
  await ensurePane(page, 'explorer-panel');
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
  await openChatOptions(page);
  await page.getByRole('button', { name: 'Send with file', exact: true }).click();
  await check(page.locator('#chat-transcript')).toContainText('The verified excerpt contains the updated and extra lines.');
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await check(page.locator('#chat')).toBeVisible();
  await page.locator('#open-workbench').click();
  // Refresh the already-selected project before reopening the edited file.
  // The resumed tab can also be restoring focus to a bookmarked file.
  await Promise.all([
    page.waitForResponse(response => response.request().headers()['forge-destination'] === 'ide.inspect'
      && response.status() === 200),
    page.getByRole('button', { name: 'Refresh project', exact: true }).click(),
  ]);
  await check(page.locator('#status')).toContainText('Partial inspection:');
  await ensurePane(page, 'explorer-panel');
  await page.locator('#project-query').fill('managed.txt');
  await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'managed.txt' }).click();
  await check(source.locator('[data-line="3"]')).toContainText('extra line');
  await check(dock.locator('#workbench-write-status')).toContainText(/Applied · rev [0-9a-f]{8}/);
  await dock.getByRole('button', { name: 'Undo edit', exact: true }).click();
  await check(source.locator('[data-line="2"]')).toContainText('old line');
  await check(source.locator('[data-line="3"]')).toContainText('omega');
  await check(dock.locator('#workbench-write-status')).toContainText(/Reverted · rev [0-9a-f]{8}/);

  await page.getByRole('button', { name: 'Edit file', exact: true }).click();
  const editor = page.getByRole('textbox', { name: 'File contents' });
  const original = await editor.inputValue();
  assert.match(original, /old line/);
  const edited = original.replace('old line', 'saved with direct editor');
  await editor.fill(edited);
  await check(page.locator('#document-status')).toHaveText('Unsaved changes');
  await page.locator('#open-chat').click();
  await page.locator('#open-workbench').click();
  await check(editor).toHaveValue(edited);
  await page.getByRole('button', { name: 'Save file', exact: true }).click();
  await check(source).toContainText('saved with direct editor');
  await check(dock.locator('#workbench-write-status')).toContainText(/Saved · rev [0-9a-f]{8}/);
  await page.getByRole('button', { name: 'Undo edit', exact: true }).click();
  await check(source.locator('[data-line="2"]')).toContainText('old line');
}

async function homeJourney(page, traffic) {
  process.stderr.write('QA_CHECK portal_home_open\n');
  const context = page.context();
  const pages = context.pages();
  const ideUrl = page.url();
  const originalViewport = page.viewportSize();
  const homeUrl = new URL('/home', ideUrl).href;
  const tabKey = () => page.evaluate(() => sessionStorage.getItem('forge-workspace-tab'));
  const originalTab = await tabKey();
  assert.match(originalTab, /^[a-f0-9]{32}$/);
  const originalDraft = await page.locator('#chat-draft').inputValue();
  const counts = () => ['resume', 'pair', 'inspect', 'source', 'other'].map(traffic.count);
  const before = counts();
  const brandLink = page.locator('header').getByRole('link', { name: 'Forge IDE', exact: true });
  await check(brandLink).toBeVisible();
  await check(brandLink).toHaveAttribute('href', '/home');
  await check(brandLink).toHaveAttribute('data-workspace-navigation', '');
  await check(brandLink).not.toHaveAttribute('target', /.+/);
  const brandBounds = await brandLink.boundingBox();
  assert.ok(brandBounds && brandBounds.width >= 44 && brandBounds.height >= 44,
    'the Forge name has a phone-sized touch target');
  const homeLink = page.locator('#ide-navigation').getByRole('link', { name: 'Home', exact: true });
  await check(homeLink).toBeVisible();
  await check(homeLink).toHaveAttribute('href', '/home');
  await check(homeLink).toHaveAttribute('data-workspace-navigation', '');
  await check(homeLink).not.toHaveAttribute('target', /.+/);
  const bounds = await homeLink.boundingBox();
  assert.ok(bounds && bounds.width >= 44 && bounds.height >= 44, 'the IDE Home link has a phone-sized touch target');
  await brandLink.focus();
  await check(brandLink).toBeFocused();

  async function open(href, activate) {
    const target = new URL(href, homeUrl);
    const documentUrl = new URL(target);
    documentUrl.hash = '';
    const [response] = await Promise.all([
      page.waitForResponse(response => response.url() === documentUrl.href && response.request().isNavigationRequest(), { timeout: 5_000 }),
      activate(),
    ]);
    assert.equal(response.status(), 200);
    await check(page).toHaveURL(target.href);
    assert.deepEqual(context.pages(), pages, 'portal navigation uses the current tab');
    if (target.hash) {
      await check(page.locator(target.hash)).toBeVisible();
    }
  }

  await open('/home', () => page.keyboard.press('Enter'));
  assert.equal(await tabKey(), originalTab, 'Home keeps the same workspace key');
  await check(page.getByRole('heading', { level: 1, name: 'Forge portal', exact: true })).toBeVisible();
  await check(page.locator('main').getByRole('heading', { level: 2 })).toHaveText([
    'Tools', 'Design documentation',
  ]);
  await check(page.locator('header').getByRole('link', { name: 'GitHub', exact: true }))
    .toHaveAttribute('href', 'https://github.com/roachmaster/homelab');
  await check(page.getByRole('link', { name: 'Back to Forge', exact: true })).toHaveCount(0);
  await check(page.locator('.home-footer').getByRole('link', { name: 'Homelab on GitHub', exact: true }))
    .toHaveAttribute('href', 'https://github.com/roachmaster/homelab');
  await check(page.locator('script')).toHaveCount(0);
  assert.deepEqual(counts(), before, 'Home performs no Forge gateway dispatches');

  process.stderr.write('QA_CHECK portal_home_layout\n');
  const launchers = page.locator('#home-tools .home-launcher');
  const docs = page.locator('#home-docs .home-doc-link');
  const primary = page.getByRole('link', { name: 'Open Forge IDE', exact: true });
  const secondary = page.getByRole('link', { name: 'Browse design docs', exact: true });
  await check(launchers).toHaveCount(3);
  await check(docs).toHaveCount(5);
  await check(launchers.locator('svg[aria-hidden="true"]')).toHaveCount(3);
  await primary.focus();
  assert.equal(await primary.evaluate(link => {
    const style = getComputedStyle(link);
    const channels = color => color.match(/[\d.]+/g).map(Number);
    const foreground = channels(style.color);
    const background = channels(style.backgroundColor);
    const luminance = values => values.slice(0, 3).map(channel => {
      const value = channel / 255;
      return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    }).reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
    const lights = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
    return (background.length === 3 || background[3] === 1)
      && (lights[0] + 0.05) / (lights[1] + 0.05) >= 4.5
      && link.matches(':focus-visible') && Number.parseFloat(style.outlineWidth) >= 2;
  }), true, 'the primary launch action has an opaque readable fill and visible keyboard focus');
  for (const width of [375, 390]) {
    await page.setViewportSize({ width, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
      true, 'Home fits the phone viewport');
    const primaryBounds = await primary.boundingBox();
    const secondaryBounds = await secondary.boundingBox();
    assert.ok(primaryBounds && secondaryBounds
      && Math.abs(primaryBounds.y - secondaryBounds.y) <= 1
      && primaryBounds.x + primaryBounds.width <= secondaryBounds.x + 1,
    'primary and secondary Home actions share one row on phones');
    assert.equal(await launchers.evaluateAll(nodes => {
      const boxes = nodes.map(node => node.getBoundingClientRect());
      return boxes.slice(1).every((box, index) => box.top >= boxes[index].bottom - 1);
    }), true, 'Home launch cards stack on phones');
    assert.equal(await page.locator('main a, header a').evaluateAll(links =>
      links.every(link => {
        const bounds = link.getBoundingClientRect();
        return bounds.width >= 44 && bounds.height >= 44;
      })), true, 'Home links have phone-sized touch targets');
  }
  await screenshot(page, 'ide-portal-home-phone.png', { home: true });
  await page.setViewportSize({ width: 1440, height: 1000 });
  assert.equal(await docs.evaluateAll(([first, second]) => {
    const left = first.getBoundingClientRect();
    const right = second.getBoundingClientRect();
    return Math.abs(left.top - right.top) <= 1 && left.right <= right.left + 1 && right.right <= innerWidth + 1;
  }), true, 'compact Home documentation links use multiple desktop columns');
  const firstLauncher = launchers.first();
  const launcherSurface = await firstLauncher.evaluate(link => getComputedStyle(link).backgroundColor);
  const documentSurface = await docs.first().evaluate(link => getComputedStyle(link).backgroundColor);
  assert.notEqual(launcherSurface, documentSurface, 'launchers have distinct visual weight from compact documentation rows');
  await firstLauncher.hover();
  await check.poll(() => firstLauncher.evaluate(link => getComputedStyle(link).backgroundColor))
    .not.toBe(launcherSurface);
  await page.keyboard.press('Tab');
  await firstLauncher.focus();
  assert.equal(await firstLauncher.evaluate(link =>
    link.matches(':focus-visible') && Number.parseFloat(getComputedStyle(link).outlineWidth) >= 2),
  true, 'launch cards expose a visible keyboard focus state');
  await page.emulateMedia({ reducedMotion: 'reduce' });
  for (const control of [primary, firstLauncher]) {
    assert.equal(await control.evaluate(link =>
      getComputedStyle(link).transitionDuration.split(',').every(value => Number.parseFloat(value) === 0)),
    true, 'Home respects reduced motion for interactive controls');
  }
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await screenshot(page, 'ide-portal-home-desktop.png', { home: true });

  process.stderr.write('QA_CHECK portal_home_destinations\n');
  const homeMain = page.locator('main');
  const destinations = [
    ['Open Forge IDE', '/'],
    ['Browse design docs', '/design'],
    ['Forge IDE', '/'],
    ['Forge Studio', '/studio'],
    ['Room One playtest', '/playtest'],
    ['Forge plug-and-play', '/design/forge-providers'],
    ['Forge foundations', '/design#forge-foundations'],
    ['Chat and Mail', '/design#chat-and-mail'],
    ['Game development', '/design#game-development'],
    ['Wireframes & IDE screens', '/design#wireframes-and-workspace'],
  ];
  for (const [label, href] of destinations) {
    const destination = homeMain.getByRole('link', { name: label, exact: true });
    await check(destination).toHaveAttribute('href', href);
    await check(destination).not.toHaveAttribute('target', /.+/);
    await open(href, () => destination.click());
    if (href === '/') {
      await check(page.locator('#workspace-panel')).toBeVisible();
      await open('/home', () => homeLink.click());
    } else if (href === '/design') {
      const designHome = page.locator('main .intro').getByRole('link', { name: 'portal home page' });
      await check(designHome).toHaveAttribute('href', '/home');
      const documentBrand = page.locator('header a.wordmark');
      await check(documentBrand).toHaveAccessibleName('Forge IDE');
      await check(documentBrand).toHaveAttribute('href', '/home');
      await open('/home', () => documentBrand.click());
    } else {
      await page.goBack({ waitUntil: 'domcontentloaded' });
      await check(page).toHaveURL(homeUrl);
    }
  }
  await check(homeMain.getByRole('link', { name: 'Homelab on GitHub' }))
    .toHaveAttribute('href', 'https://github.com/roachmaster/homelab');

  if (originalViewport) await page.setViewportSize(originalViewport);
  await check(primary).toHaveAttribute('href', '/');
  await open('/', () => primary.click());
  await check(page).toHaveURL(ideUrl);
  await check(page.locator('#workspace-panel')).toBeVisible();
  await check(page.locator('#chat-draft')).toHaveValue(originalDraft);
  assert.equal(await tabKey(), originalTab, 'returning from Home restores the same tab workspace');
  assert.equal(page.workers().length, 1, 'returning from Home starts one Forge worker');
  await open('/home', () => brandLink.click());
  await open('/', () => primary.click());
  await check(page.locator('#workspace-panel')).toBeVisible();
  await check(page.locator('#chat-draft')).toHaveValue(originalDraft);
  assert.equal(await tabKey(), originalTab, 'clicking the Forge name preserves the workspace');
}

async function designJourney(page, traffic) {
  process.stderr.write('QA_CHECK design_open\n');
  const link = page.locator('#ide-navigation').getByRole('link', { name: 'Design', exact: true });
  const originalUrl = page.url();
  const originalViewport = page.viewportSize();
  const context = page.context();
  const pages = context.pages();
  const tabKey = () => page.evaluate(() => sessionStorage.getItem('forge-workspace-tab'));
  const originalTab = await tabKey();
  assert.match(originalTab, /^[a-f0-9]{32}$/);
  const designUrl = new URL('/design', originalUrl).href;
  const styleUrl = new URL('/design/style.css', originalUrl).href;
  const counts = () => ['resume', 'pair', 'inspect', 'source', 'other'].map(traffic.count);
  const before = counts();
  await check(link).toBeVisible();
  await check(link).toHaveAttribute('href', '/design');
  await check(link).not.toHaveAttribute('target', /.+/);
  await check(link).toHaveAttribute('data-workspace-navigation', '');
  const bounds = await link.boundingBox();
  assert.ok(bounds && bounds.width >= 44 && bounds.height >= 44, 'Design has a phone-sized touch target');
  await link.focus();
  await check(link).toBeFocused();
  const [documentResponse, stylesheet] = await Promise.all([
    page.waitForEvent('response', { predicate: response => response.url() === designUrl && response.request().isNavigationRequest(), timeout: 5_000 }),
    page.waitForEvent('response', { predicate: response => response.url() === styleUrl, timeout: 5_000 }),
    page.keyboard.press('Enter'),
  ]);
  assert.equal(documentResponse.status(), 200);
  assert.match(documentResponse.headers()['content-type'] ?? '', /^text\/html\b/);
  assert.equal(stylesheet.status(), 200);
  assert.match(stylesheet.headers()['content-type'] ?? '', /^text\/css\b/);
  await check(page).toHaveURL(designUrl);
  assert.deepEqual(context.pages(), pages, 'Design opens without creating a second page');
  assert.equal(await tabKey(), originalTab, 'Design uses the same per-tab workspace key');
  await check(page.getByRole('heading', { level: 1, name: 'Forge IDE design', exact: true })).toBeVisible();
  await check(page.getByRole('heading', { name: 'Workspace recovery', exact: true })).toBeVisible();
  await check(page.getByRole('link', { name: 'Back to Forge', exact: true })).toHaveAttribute('href', '/');
  const designIndex = page.getByRole('navigation', { name: 'Design sections' });
  await check(designIndex.getByRole('heading', { level: 2 })).toHaveText([
    'Forge foundations', 'Chat and Mail', 'Game development', 'Wireframes and workspace',
  ]);
  const providerLink = designIndex.getByRole('link').first();
  await check(providerLink).toHaveText('Forge plug-and-play');
  await check(providerLink).toHaveAttribute('href', '/design/forge-providers');
  await check(providerLink).not.toHaveAttribute('target', /.+/);
  await check(page.locator('#phone figure.wireframe')).toHaveCount(3);
  await check(page.locator('#desktop figure.wireframe')).toHaveCount(2);

  process.stderr.write('QA_CHECK design_wireframes\n');
  const names = ['chat-phone', 'workbench-phone', 'workbench-project-options', 'chat-desktop', 'workbench-desktop'];
  const images = page.locator('figure.wireframe img');
  await check(images).toHaveCount(names.length);
  for (const name of names) {
    const path = `/design/forge-ide-${name}.png`;
    const image = page.locator(`figure.wireframe img[src="${path}"]`);
    await check(image).toHaveAttribute('alt', /\S/);
    await check(page.locator(`figure.wireframe a[href="${path}"]`).filter({ has: page.locator('img') }))
      .toHaveAttribute('target', '_blank');
    await image.scrollIntoViewIfNeeded();
    await check.poll(() => image.evaluate(node => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0)).toBe(true);
  }

  process.stderr.write('QA_CHECK design_phone_layout\n');
  for (const viewport of [{ width: 390, height: 664 }, { width: 375, height: 667 }]) {
    await page.setViewportSize(viewport);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'Design fits the phone viewport');
    assert.equal(await designIndex.getByRole('region').evaluateAll(panels => {
      const boxes = panels.map(panel => panel.getBoundingClientRect());
      return boxes.slice(1).every((box, index) => box.top >= boxes[index].bottom - 1);
    }), true, 'design index categories stack on phones');
    assert.equal(await images.evaluateAll(nodes => nodes.every(node => {
      const image = node.getBoundingClientRect();
      return image.width > 0 && image.height > 0 && image.left >= -1 && image.right <= innerWidth + 1;
    })), true, 'each wireframe fits horizontally');
    assert.equal(await page.locator('#phone figure.wireframe').evaluateAll(figures => {
      const boxes = figures.map(figure => figure.getBoundingClientRect());
      return boxes.slice(1).every((box, index) => box.top >= boxes[index].bottom - 1);
    }), true, 'phone wireframe cards stack vertically');
  }
  process.stderr.write('QA_CHECK design_desktop_layout\n');
  await page.setViewportSize({ width: 1440, height: 1000 });
  assert.equal(await page.locator('#desktop figure.wireframe').evaluateAll(([first, second]) => {
    const left = first.getBoundingClientRect();
    const right = second.getBoundingClientRect();
    return Math.abs(left.top - right.top) <= 1 && left.right <= right.left + 1 && right.right <= innerWidth + 1;
  }), true, 'desktop wireframe cards display side by side');
  assert.equal(await designIndex.getByRole('region').evaluateAll(([first, second]) =>
    first.offsetTop === second.offsetTop && first.offsetLeft < second.offsetLeft),
  true, 'design index categories display side by side on desktop');
  await providerLink.focus();
  await check(providerLink).toBeFocused();
  await page.keyboard.press('Enter');
  const providerUrl = new URL('/design/forge-providers', designUrl).href;
  await check(page).toHaveURL(providerUrl);
  assert.deepEqual(context.pages(), pages, 'the provider guide opens in the same tab');
  await check(page.getByRole('heading', { level: 1 })).toHaveText('Forge plug-and-play');
  await page.setViewportSize({ width: 375, height: 667 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
    'the provider guide fits the phone viewport');
  const contents = page.getByRole('navigation', { name: 'On this page' });
  await check(contents.getByRole('link')).toHaveText([
    'Wiring sequence', 'Factory and provider roles', 'Working implementations', 'Verify a change',
  ]);
  await contents.getByRole('link', { name: 'Verify a change' }).click();
  await check(page).toHaveURL(`${providerUrl}#verification`);
  await check(page.locator('#verification')).toHaveText('Verify a change');
  await page.getByRole('link', { name: 'Back to Design', exact: true }).click();
  await check(page).toHaveURL(`${designUrl}#forge-foundations`);
  await previousChatsDesignJourney(page, designUrl);
  await gameEngineDesignJourney(page, designUrl);
  assert.deepEqual(counts(), before, 'Design does not send requests through the Forge gateway');
  if (originalViewport) await page.setViewportSize(originalViewport);
  await page.getByRole('link', { name: 'Back to Forge', exact: true }).click();
  await check(page).toHaveURL(originalUrl);
  assert.equal(await tabKey(), originalTab, 'the IDE returns to the same tab workspace');
  assert.deepEqual(context.pages(), pages, 'returning from Design uses the existing page');
  await check(page.locator('#workspace-panel')).toBeVisible();
  assert.equal(page.workers().length, 1, 'returning from Design starts one Forge worker');
}

async function designLocalRestoreJourney(page) {
  process.stderr.write('QA_CHECK design_local_workspace_restore\n');
  const originalUrl = page.url();
  const designUrl = new URL('/design', originalUrl).href;
  const draft = page.locator('#chat-draft');
  const transcript = page.locator('#chat-transcript');
  const prompt = 'This sent prompt belongs in the remote workspace';
  const reply = 'This completed reply belongs in the remote workspace';
  const context = page.context();
  const gateway = '**/forge/v1/dispatch';
  const replyToChat = async route => {
    const body = route.request().postDataJSON();
    if (body?.kind !== 'chat') return route.continue();
    await route.fulfill({ json: {
      protocol_version: 1,
      event: {
        kind: 'chat_result', request_id: body.request.request_id,
        outcome: { kind: 'answer', content: reply },
      },
    } });
  };
  await context.route(gateway, replyToChat);
  try {
    await draft.fill(prompt);
    await page.locator('#send-chat').click();
    await check(transcript).toContainText(prompt);
    await check(transcript).toContainText(reply);
  } finally {
    await context.unroute(gateway, replyToChat);
  }

  const text = 'Restore this updated draft when the remote checkpoint is unavailable';
  await draft.fill(text);
  await page.getByRole('link', { name: 'Design', exact: true }).click();
  await check(page).toHaveURL(designUrl);
  const handoff = await page.evaluate(() => JSON.parse(sessionStorage.getItem('forge-workspace-navigation')));
  assert.equal(handoff?.version, 1);
  assert.equal(handoff.tab, await page.evaluate(() => sessionStorage.getItem('forge-workspace-tab')));
  assert.equal(checkpointHasDraft(handoff.snapshot, text), true);
  assert.ok(handoff.snapshot.chat.conversations.every(conversation => conversation.messages.length === 0),
    'the local checkpoint excludes completed transcript bodies');
  assert.equal(handoff.snapshot.chat.pending_run ?? null, null);

  const workspaceRoute = '**/forge/v1/dispatch/workspace';
  let rejectedRestore = false;
  const rejectRestore = async route => {
    if (route.request().postDataJSON()?.operation === 'restore') {
      rejectedRestore = true;
      await route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
    } else {
      await route.continue();
    }
  };
  await context.route(workspaceRoute, rejectRestore);
  try {
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await check(page).toHaveURL(originalUrl);
    await check(draft).toBeVisible();
    await check(draft).toHaveValue(text);
    await check(transcript).not.toContainText(reply);
    assert.equal(rejectedRestore, true, 'browser Back restores the local draft despite a failed workspace request');
  } finally {
    await context.unroute(workspaceRoute, rejectRestore);
  }
}

async function previousChatsDesignJourney(design, designUrl) {
  process.stderr.write('QA_CHECK design_previous_chats\n');
  await design.getByRole('navigation', { name: 'Design sections' }).getByRole('link', { name: 'Previous chats', exact: true }).click();
  await check(design).toHaveURL(`${designUrl}#previous-chats`);
  const proposal = design.locator('#previous-chats');
  const menu = proposal.locator('#previous-chat-menu');
  const summary = menu.locator('summary');
  const title = proposal.locator('#previous-chat-title');
  const transcript = proposal.locator('#previous-chat-transcript');
  const draft = proposal.locator('#previous-chat-draft');
  const archive = proposal.locator('#previous-chat-archive');
  const newChat = proposal.locator('#previous-chat-new');
  const selected = menu.locator('[data-preview-chat][aria-current="true"]');
  const row = name => menu.locator(`button[data-preview-chat="${name}"]`);
  async function choose(name, heading) {
    if (!await menu.evaluate(details => details.open)) await summary.click();
    await row(name).click();
    await check(menu).toHaveJSProperty('open', false);
    await check(selected).toHaveCount(1);
    await check(row(name)).toHaveAttribute('aria-current', 'true');
    await check(title).toHaveText(heading);
    await check(transcript).toContainText(/\S/);
  }

  await check(proposal.getByRole('heading', { name: 'Previous chats', exact: true })).toBeVisible();
  await check(menu).toHaveJSProperty('open', true);
  await check(summary).toHaveText('Previous chats');
  await check(row('workbench')).toHaveAttribute('aria-current', 'true');
  await check(title).toHaveText('Summarize Workbench');
  await check(archive).toBeHidden();
  await check(draft).toHaveValue('Add a note about Explorer.');
  const initialTranscript = await transcript.innerText();
  await choose('source', 'Explain source verification');
  await check(transcript).not.toHaveText(initialTranscript, { useInnerText: true });
  const sourceTranscript = await transcript.innerText();
  const savedDraft = 'Keep this draft while viewing another sample';
  await draft.fill(savedDraft);
  await choose('backup', 'Plan a backup');
  await check(transcript).not.toHaveText(sourceTranscript, { useInnerText: true });
  await check(draft).not.toHaveValue(savedDraft);
  await choose('source', 'Explain source verification');
  await check(transcript).toHaveText(sourceTranscript, { useInnerText: true });
  await check(draft).toHaveValue(savedDraft);

  await summary.click();
  await row('backup').focus();
  await check(row('backup')).toBeFocused();
  await design.keyboard.press('Escape');
  await check(menu).toHaveJSProperty('open', false);
  await check(summary).toBeFocused();

  process.stderr.write('QA_CHECK design_previous_chats_archive\n');
  await choose('archive', 'Inspect a Rust project');
  await check(archive).toBeVisible();
  await check(draft).toBeHidden();
  await check(proposal.locator('#previous-chat-manage')).toBeHidden();
  const archivedTranscript = await transcript.innerText();
  await archive.getByRole('button', { name: 'Continue as new chat', exact: true }).click();
  await check(archive).toBeHidden();
  await check(transcript).toHaveText(archivedTranscript, { useInnerText: true });
  await check(draft).toBeEditable();
  await check(draft).toHaveValue('');
  const continuationDraft = 'Edit only this new chat copy';
  await draft.fill(continuationDraft);
  await check(draft).toHaveValue(continuationDraft);
  await choose('archive', 'Inspect a Rust project');
  await check(archive).toBeVisible();
  await check(draft).toBeHidden();
  await check(transcript).toHaveText(archivedTranscript, { useInnerText: true });
  await newChat.click();
  await check(draft).toBeEditable();
  await check(draft).toHaveValue('');
  await check(transcript.locator('[data-role]')).toHaveCount(0);

  process.stderr.write('QA_CHECK design_previous_chats_phone\n');
  if (!await menu.evaluate(details => details.open)) await summary.click();
  await check(menu).toHaveJSProperty('open', true);
  for (const viewport of [{ width: 390, height: 664 }, { width: 375, height: 667 }]) {
    await design.setViewportSize(viewport);
    assert.equal(await proposal.evaluate(section => {
      const controls = section.querySelectorAll('#previous-chat-menu button, #previous-chat-draft');
      return document.documentElement.scrollWidth <= innerWidth + 1 && [...controls].every(node => {
        const bounds = node.getBoundingClientRect();
        return bounds.width === 0 || (bounds.left >= -1 && bounds.right <= innerWidth + 1);
      });
    }), true, 'the open Previous chats menu and editable composer fit the phone');
  }

  process.stderr.write('QA_CHECK design_chat_retention\n');
  await choose('workbench', 'Summarize Workbench');
  const retention = proposal.locator('#previous-chat-retention');
  const management = proposal.locator('#previous-chat-manage');
  const manage = management.locator('summary');
  await check(retention).toContainText('Nov 30, 2026');
  await summary.click();
  await check(menu).toHaveJSProperty('open', true);
  await manage.click();
  await check(menu).toHaveJSProperty('open', false);
  await check(management).toHaveJSProperty('open', true);
  await proposal.locator('#previous-chat-expire').click();
  const expiration = design.locator('#previous-chat-expiration-dialog');
  await check(expiration).toBeVisible();
  await expiration.locator('#previous-chat-expiration-date').fill('2027-01-10');
  await expiration.getByRole('button', { name: 'Save expiration', exact: true }).click();
  await check(expiration).toBeHidden();
  await check(retention).toContainText('Jan 10, 2027');
  await manage.click();
  await proposal.locator('#previous-chat-keep').click();
  await check(retention).toContainText('No expiration scheduled');

  await manage.click();
  await proposal.locator('#previous-chat-delete').click();
  const deletion = design.locator('#previous-chat-delete-dialog');
  const cancelDelete = deletion.getByRole('button', { name: 'Cancel', exact: true });
  await check(deletion).toContainText('Summarize Workbench');
  await check(cancelDelete).toBeFocused();
  await cancelDelete.click();
  await check(deletion).toBeHidden();
  await check(title).toHaveText('Summarize Workbench');
  await proposal.locator('#previous-chat-delete').click();
  await deletion.locator('#previous-chat-confirm-delete').click();
  await check(row('workbench')).toHaveCount(0);
  await check(title).toHaveText('Explain source verification');
  await check(draft).toHaveValue(savedDraft);
  await check(summary).toBeFocused();

  process.stderr.write('QA_CHECK design_chat_history_contract\n');
  await proposal.locator('a[href="/design/chat-history"]').click();
  await check(design).toHaveURL(`${new URL(designUrl).origin}/design/chat-history`);
  await check(design.getByRole('heading', { level: 1 })).toHaveText('Previous chats technical design');
  const diagrams = design.locator('.contract-sequences img');
  await check(diagrams).toHaveCount(4);
  for (const diagram of await diagrams.all()) {
    await diagram.scrollIntoViewIfNeeded();
    await check.poll(() => diagram.evaluate(img => img.naturalWidth)).toBeGreaterThan(0);
  }
  assert.equal(await design.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
    true, 'contract and diagrams scroll inside the phone page');

  process.stderr.write('QA_CHECK design_postgres_dispatch\n');
  await design.getByRole('link', { name: 'PostgreSQL dispatch', exact: true }).click();
  await check(design).toHaveURL(`${new URL(designUrl).origin}/design/chat-dispatch`);
  await check(design.getByRole('heading', { level: 1 })).toHaveText(/PostgreSQL.*dispatch/i);
  await check(design.locator('.intro')).toContainText('Dispatch design.');
  for (const section of ['sequence', 'data-access', 'contracts', 'rollout']) {
    await check(design.locator(`#${section} h2`)).toBeVisible();
  }
  const inferenceDiagram = design.locator('img[src="/design/chat-dispatch-sequence.svg"]');
  await inferenceDiagram.scrollIntoViewIfNeeded();
  await check.poll(() => inferenceDiagram.evaluate(img => img.naturalWidth)).toBeGreaterThan(0);
  await check(design.locator('a:has(img[src="/design/chat-dispatch-sequence.svg"])'))
    .toHaveAttribute('href', '/design/chat-dispatch-sequence.svg');
  for (const width of [375, 390, 1440]) {
    await design.setViewportSize({ width, height: 844 });
    assert.equal(await design.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
      true, 'dispatch design fits the viewport with a separately scrollable diagram');
  }
  await design.getByRole('link', { name: /back to design/i }).click();
  const landingLink = design.getByRole('link', { name: 'PostgreSQL dispatch', exact: true });
  await check(landingLink).toHaveAttribute('href', '/design/chat-dispatch');
}

async function gameEngineDesignJourney(page, designUrl) {
  const pages = page.context().pages();
  const link = page.getByRole('link', { name: 'Game engine design', exact: true });
  await check(page.getByRole('link', { name: 'Room One playtest', exact: true }))
    .toHaveAttribute('href', '/playtest');
  await check(link).toHaveAttribute('href', '/design/game-studio');
  await link.click();
  const gameDesignUrl = new URL('/design/game-studio', designUrl).href;
  await check(page).toHaveURL(gameDesignUrl);
  assert.deepEqual(page.context().pages(), pages, 'the game engine report opens in Design’s tab');

  const report = page.getByRole('main');
  await check(report.getByRole('heading', {
    level: 1, name: 'Extending Homelab with a Rust Game Engine', exact: true,
  })).toBeVisible();
  for (const name of [
    'What Homelab already provides', 'Package layout and ownership', 'In-process engine API',
    'Platform and library APIs', 'Forge project and build APIs', 'Portal integration',
    'Delivery and acceptance',
  ]) {
    await check(report.getByRole('heading', { level: 2, name, exact: true })).toBeVisible();
  }
  await check(report.getByRole('table')).toHaveCount(5);
  const diagrams = report.getByRole('img');
  await check(diagrams).toHaveCount(2);
  for (const diagram of await diagrams.all()) {
    await check(diagram).toHaveAttribute('src', /\.svg$/);
    await check(diagram).toHaveAttribute('alt', /\S/);
    await diagram.scrollIntoViewIfNeeded();
    await check.poll(() => diagram.evaluate(node => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0))
      .toBe(true);
  }
  for (const width of [320, 390, 1440]) {
    await page.setViewportSize({ width, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
      true, `the game engine report fits the ${width}px viewport`);
  }

  const playtest = report.getByRole('link', { name: 'Open browser playtest', exact: true });
  await check(playtest).toHaveAttribute('href', '/playtest');
  await page.setViewportSize({ width: 320, height: 720 });
  const context = page.context();
  const wasmUrl = new URL('/app.wasm', designUrl).href;
  const workerUrl = new URL('/playtest/assets-worker.js', designUrl).href;
  const cookieName = 'forge_fixture_playtest_asset';
  // Model an access proxy's same-origin cookie without touching the Forge session.
  await context.addCookies([{
    name: cookieName, value: 'present', domain: new URL(wasmUrl).hostname, path: '/app.wasm',
  }]);
  try {
    const [wasm] = await Promise.all([
      page.waitForResponse(response => response.url() === wasmUrl),
      page.waitForEvent('worker', worker => worker.url() === workerUrl),
      playtest.click(),
    ]);
    assert.equal(wasm.status(), 200);
    assert.match(await wasm.request().headerValue('cookie') ?? '', /forge_fixture_playtest_asset=present/);
  } finally {
    await context.clearCookies({ name: cookieName });
  }
  await check(page).toHaveURL(new URL('/playtest', designUrl).href);
  assert.deepEqual(page.context().pages(), pages, 'the playtest opens in Design’s tab');
  await page.bringToFront();
  await check(page.getByRole('heading', { level: 1, name: 'Room One', exact: true })).toBeVisible();
  const board = page.locator('canvas#playtest-board');
  const status = page.locator('#playtest-status');
  const restart = page.getByRole('button', { name: 'Restart', exact: true });
  const assetsUrl = new URL('/playtest/assets/', designUrl);
  const cooked = (await page.request.get(new URL('manifest.json', assetsUrl).href)).ok();
  const levelGroup = page.locator('#playtest-levels');
  await check(board).toBeVisible();
  await check(status).toHaveText('Find the key.');
  if (cooked) {
    await check(levelGroup).toHaveAttribute('aria-busy', 'false');
    await check(board).toHaveAttribute('data-playtest-art', 'atlas');
  }
  for (const direction of ['up', 'down', 'left', 'right']) {
    await check(page.getByRole('button', { name: `Move ${direction}`, exact: true })).toBeEnabled();
  }
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
    true, 'the playtest fits the 320px viewport');

  // Give each short press enough real display frames for the fixed-step game
  // loop to consume it before the next press. The browser drives the runtime.
  const waitForTicks = () => page.evaluate(() => new Promise(resolve => {
    requestAnimationFrame(start => {
      const next = now => {
        if (now - start >= 50) resolve();
        else requestAnimationFrame(next);
      };
      requestAnimationFrame(next);
    });
  }));
  const keys = { U: 'ArrowUp', D: 'ArrowDown', L: 'ArrowLeft', R: 'ArrowRight' };
  const move = async sequence => {
    for (const direction of sequence) {
      await page.keyboard.press(keys[direction]);
      await waitForTicks();
    }
  };
  await page.getByRole('button', { name: 'Move right', exact: true }).click();
  await waitForTicks();
  await board.focus();
  await check(board).toBeFocused();
  await move('RDDLLDDRRDDLL');
  await check(status).toHaveText('Exit open. Reach the exit.');
  await move('RRUURRDDRRRRRRUUU');
  await check(status).toHaveText('You won!');
  await restart.click();
  await check(status).toHaveText('Find the key.');

  const debug = page.locator('#playtest-debug');
  await debug.locator('summary').click();
  const pause = debug.locator('#playtest-pause');
  const step = debug.getByRole('button', { name: 'Step', exact: true });
  const inspector = debug.locator('#playtest-inspector');
  await check(inspector).toContainText('Entities: 4');
  await check(pause).toHaveText('Pause');
  await check(step).toBeDisabled();
  await pause.click();
  await check(pause).toHaveAttribute('aria-pressed', 'true');
  const pausedTick = Number((await inspector.textContent()).match(/\bTick: (\d+)/)[1]);
  await waitForTicks();
  await check(inspector).toHaveText(new RegExp(`\\bTick: ${pausedTick}\\b`));

  await page.getByRole('button', { name: 'Move right', exact: true }).click();
  await step.click();
  await check(inspector).toHaveText(new RegExp(`\\bTick: ${pausedTick + 1}\\b`));
  await waitForTicks();
  await check(inspector).toHaveText(new RegExp(`\\bTick: ${pausedTick + 1}\\b`));
  await debug.getByRole('checkbox', { name: 'Show board overlay', exact: true }).check();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
    true, 'the open debug drawer fits the 320px viewport');
  await debug.getByRole('checkbox', { name: 'Input', exact: true }).uncheck();
  await page.getByRole('button', { name: 'Move right', exact: true }).click();
  const command = debug.getByRole('textbox', { name: 'Command', exact: true });
  await command.fill('step');
  await command.press('Enter');
  await check(inspector).toHaveText(new RegExp(`\\bTick: ${pausedTick + 2}\\b`));
  await check(debug.locator('#playtest-trace')).toContainText('Replay: incomplete');

  const [download] = await Promise.all([
    page.waitForEvent('download'),
    debug.getByRole('button', { name: 'Export trace', exact: true }).click(),
  ]);
  assert.equal(download.suggestedFilename(), 'room-one-trace.json');
  const trace = JSON.parse(await readFile(await download.path(), 'utf8'));
  assert.equal(trace.replayComplete, false, 'masked movement makes the replay export incomplete');
  await download.delete();

  if (cooked) {
    const traversal = page.getByRole('button', { name: 'Traversal', exact: true });
    const roomOne = page.getByRole('button', { name: 'Room One', exact: true });
    const jump = page.getByRole('button', { name: 'Jump', exact: true });
    const loadStatus = page.locator('#playtest-load-status');
    const levelUrl = new URL('levels/traversal-yard.json', assetsUrl).href;
    const corruptLevel = route => route.fulfill({
      status: 200, contentType: 'application/json', body: '{}',
    });
    await page.route(levelUrl, corruptLevel);
    try {
      await traversal.click();
      await check(loadStatus).toContainText('Could not open Traversal Yard');
      await check(roomOne).toHaveAttribute('aria-pressed', 'true');
      await check(jump).toBeHidden();
    } finally {
      await page.unroute(levelUrl, corruptLevel);
    }

    // A prepared level remains staged while paused. One manual tick admits it.
    await traversal.click();
    await check(loadStatus).toContainText('Resume or step to open it.');
    await check(page.getByRole('heading', { level: 1 })).toHaveText('Room One');
    await step.click();
    await check(page.getByRole('heading', { level: 1 })).toHaveText('Traversal Yard');
    await check(loadStatus).toHaveText('Entered Traversal Yard.');
    await check(debug.locator('#playtest-timing')).toContainText(/Traversal Yard handoff · WASM linear memory \d+ → \d+ bytes \(peak\)/);
    await check(inspector).toContainText('Collision bodies:');
    await check(inspector).toContainText('Downward ray:');
    await check(traversal).toHaveAttribute('aria-pressed', 'true');
    await check(jump).toBeEnabled();
    await check(page.getByRole('button', { name: 'Move up', exact: true })).toBeHidden();
    await check(board).toHaveAttribute('data-playtest-art', 'atlas');
    await check(inspector).toContainText('grounded: yes');
    const playerY = async () => Number((await inspector.textContent()).match(/\bPlayer: \([^,]+, ([^)]+)\)/)[1]);
    const groundedY = await playerY();
    await board.focus();
    await page.keyboard.press('Space');
    await step.click();
    await check(inspector).toContainText('Pose: jump');
    await check(inspector).toContainText('grounded: no');
    assert((await playerY()) > groundedY, 'the fixed tick raises the player after a Space jump');
    await pause.click();
    await check(inspector).toContainText('grounded: yes');
    await pause.click();
    assert.equal(await playerY(), groundedY);
    await jump.click();
    await step.click();
    await check(inspector).toContainText('grounded: no');

    const [levelTrace] = await Promise.all([
      page.waitForEvent('download'),
      debug.getByRole('button', { name: 'Export trace', exact: true }).click(),
    ]);
    const activation = JSON.parse(await readFile(await levelTrace.path(), 'utf8')).activation;
    assert.equal(activation.slot, 2);
    assert.match(activation.manifestSha256, /^[a-f0-9]{64}$/);
    assert(Number.isInteger(activation.priorWorldTick));
    await levelTrace.delete();

    const [sessionReplay] = await Promise.all([
      page.waitForEvent('download'),
      debug.getByRole('button', { name: 'Export replay', exact: true }).click(),
    ]);
    assert.equal(sessionReplay.suggestedFilename(), 'forge-playtest-replay.json');
    const replayBytes = await readFile(await sessionReplay.path());
    const replay = JSON.parse(replayBytes);
    assert.equal(replay.version, 1);
    assert.equal(replay.complete, true, 'the session journal is independent of the debug trace mask');
    const [outgoing, incoming] = replay.segments.slice(-2);
    assert.equal(outgoing.level_slot, 1);
    assert.equal(incoming.level_slot, 2);
    assert.equal(outgoing.transition.kind, 'level');
    assert.equal(outgoing.transition.boundary_tick, activation.priorWorldTick);
    assert.equal(outgoing.transition.incoming_epoch, incoming.epoch);
    assert.equal(incoming.manifest_sha256, activation.manifestSha256);
    assert(incoming.inputs.some(run => run.input === 'jump'));
    await sessionReplay.delete();

    await roomOne.click();
    await check(loadStatus).toContainText('Resume or step to open it.');
    await step.click();
    await check(page.getByRole('heading', { level: 1 })).toHaveText('Room One');
    await check(roomOne).toHaveAttribute('aria-pressed', 'true');
    await check(page.getByRole('button', { name: 'Move up', exact: true })).toBeEnabled();

    const imageUrl = new URL('atlas/sheet.png', assetsUrl).href;
    const missingAtlas = route => route.fulfill({ status: 404, body: '' });
    await page.route(imageUrl, missingAtlas);
    try {
      const [unavailable] = await Promise.all([
        page.waitForResponse(response => response.url() === imageUrl),
        page.reload(),
      ]);
      assert.equal(unavailable.status(), 404);
      await check(status).toHaveText('Find the key.');
      await check(board).toHaveAttribute('data-playtest-art', 'primitive');
      await check(page.getByRole('button', { name: 'Move up', exact: true })).toBeEnabled();
    } finally {
      await page.unroute(imageUrl, missingAtlas);
    }

    await debug.locator('summary').click();
    const archiveUrl = new URL(`archive/${incoming.manifest_sha256}/levels/traversal-yard.json`, assetsUrl).href;
    const [restored] = await Promise.all([
      page.waitForResponse(response => response.url() === archiveUrl),
      debug.getByLabel('Import replay').setInputFiles({ name: 'replay.json', mimeType: 'application/json', buffer: replayBytes }),
    ]);
    assert.equal(restored.status(), 200);
    await check(debug.locator('#playtest-replay-status')).toHaveText('Replay ready.');
    const position = debug.locator('#playtest-replay-position');
    await check(position).toHaveText('Segment 1 · Tick 0');
    await check(debug.getByRole('img', { name: 'Replay game board' })).toBeVisible();
    await debug.getByRole('button', { name: 'Step replay' }).click();
    await check(position).not.toHaveText('Segment 1 · Tick 0');
    await debug.getByRole('button', { name: 'Restart replay' }).click();
    await check(position).toHaveText('Segment 1 · Tick 0');
    await debug.getByRole('button', { name: 'Play replay' }).click();
    await check(position).not.toHaveText('Segment 1 · Tick 0');
    await check(page.getByRole('heading', { level: 1 })).toHaveText('Room One');
    await debug.getByRole('button', { name: 'Restart replay' }).click();
    await debug.getByLabel('Import replay').setInputFiles({ name: 'bad.json', mimeType: 'application/json', buffer: Buffer.from('{}') });
    await check(debug.locator('#playtest-replay-status')).toHaveText('This replay file is invalid or incomplete.');
    await check(position).toHaveText('Segment 1 · Tick 0');
  }

  await page.getByRole('link', { name: 'Back to game engine design', exact: true }).click();
  await check(page).toHaveURL(gameDesignUrl);
  assert.deepEqual(page.context().pages(), pages, 'the playtest returns in the same tab');
  await page.getByRole('link', { name: 'Back to Design', exact: true }).click();
  await check(page).toHaveURL(designUrl);
  await check(link).toBeVisible();
  assert.deepEqual(page.context().pages(), pages, 'the game engine report returns in the same tab');
}

async function themeColor(page, token) {
  return page.evaluate(name => {
    const sample = document.createElement('span');
    sample.style.color = `var(--${name})`;
    document.body.append(sample);
    const color = getComputedStyle(sample).color;
    sample.remove();
    return color;
  }, token);
}

async function chatJourney(page, context) {
  const gateway = '**/forge/v1/dispatch';
  const transcript = page.locator('#chat-transcript');
  const draft = page.locator('#chat-draft');
  const select = page.locator('#chat-conversation');
  const send = page.locator('#send-chat');
  const replyMarkdown = '## Reply\n\nReply `<img src=x onerror=alert(1)>` with [the guide](https://example.com/guide).\n\n| Topic | Key |\n|---|---|\n| **What** | Virus |';
  async function assertChatShell() {
    await check(transcript).toHaveCSS('border-top-width', '0px');
    await check(transcript).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
    await check(draft).toHaveAttribute('placeholder', 'Message Forge…');
    await check(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible();
    await check(send).toHaveAttribute('aria-label', 'Send');
    const [draftBox, sendBox, transcriptBox, headerBox, tabsBox] = await Promise.all([
      draft.boundingBox(), send.boundingBox(), transcript.boundingBox(),
      page.locator('.fg-header').boundingBox(), page.locator('#ide-navigation').boundingBox(),
    ]);
    const viewport = page.viewportSize();
    assert(draftBox && sendBox && sendBox.x >= draftBox.x && sendBox.x + sendBox.width <= draftBox.x + draftBox.width + 1
      && sendBox.y >= draftBox.y && sendBox.y + sendBox.height <= draftBox.y + draftBox.height + 1,
    'Send is inside the trailing edge of the composer');
    assert(sendBox.width >= 44 && sendBox.height >= 44, 'the inline Send target remains usable by touch');
    assert(draftBox.y + draftBox.height <= viewport.height + 1, 'the composer remains inside the viewport');
    assert(transcriptBox && transcriptBox.height >= viewport.height * .4,
      'the open message stream receives the remaining viewport height');
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'the active Chat shell has no horizontal overflow');
    if (viewport.width < 640) {
      assert(headerBox && tabsBox && headerBox.height <= 56 && tabsBox.height <= 48
        && tabsBox.y + tabsBox.height <= 110,
      'mobile Chat has only two compact header/navigation bars');
      assert(transcriptBox.y >= tabsBox.y + tabsBox.height - 1
        && transcriptBox.y <= tabsBox.y + tabsBox.height + 20,
      'the conversation begins immediately below the two-bar chrome');
      for (const id of ['forge-home', 'chat-history-toggle', 'new-chat']) {
        const control = page.locator('#' + id);
        await check(control).toBeVisible();
        const box = await control.boundingBox();
        assert(box && box.width >= 44 && box.height >= 44, id + ' retains its mobile touch target');
      }
      for (const id of ['open-chat', 'open-workbench', 'open-home', 'open-design']) {
        await check(page.locator('#' + id)).toBeVisible();
      }
      const visibleTabs = page.locator('#ide-navigation > :is(button,a):visible');
      const tabCount = await visibleTabs.count();
      assert(tabCount >= 4 && tabCount <= 5, 'the admitted catalog exposes the four core tabs and optional Mail');
      for (const control of await visibleTabs.all()) {
        const box = await control.boundingBox();
        assert(box && box.width >= 44 && box.height >= 44,
          (await control.getAttribute('id')) + ' retains its mobile touch target');
      }
    }
    assert.equal(await draft.evaluate(node => {
      const channels = color => color.match(/[\d.]+/g).map(Number);
      const luminance = values => values.slice(0, 3).map(channel => {
        const value = channel / 255;
        return value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4;
      }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
      const style = getComputedStyle(node);
      let surrounding = node.parentElement;
      while (surrounding && getComputedStyle(surrounding).backgroundColor === 'rgba(0, 0, 0, 0)') surrounding = surrounding.parentElement;
      const colors = [style.borderTopColor, style.backgroundColor,
        surrounding ? getComputedStyle(surrounding).backgroundColor : 'rgb(0, 0, 0)'];
      return colors.slice(1).every(background => {
        const lights = [luminance(channels(colors[0])), luminance(channels(background))].sort((a, b) => b - a);
        return (lights[0] + .05) / (lights[1] + .05) >= 3;
      });
    }), true, 'the idle composer boundary has at least 3:1 contrast against its adjacent fills');
  }
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
        outcome: { kind: 'answer', content: sent.length === 1 ? replyMarkdown : replyMarkdown + '\n\n' + 'A longer controlled reply keeps the message stream scrolling while the composer stays reachable.\n\n'.repeat(40) },
      },
    } });
  };
  await page.locator('#open-chat').click();
  await check(page.locator('#chat')).toBeVisible();
  await check(page.locator('#ide-navigation').getByRole('link', { name: 'Design', exact: true })).toBeVisible();
  await check(page.locator('#workbench-context')).toBeHidden();
  const historyTrigger = page.getByRole('button', { name: 'Previous chats', exact: true });
  await historyTrigger.focus();
  await page.keyboard.press('Enter');
  await check(page.getByRole('dialog', { name: 'Conversations', exact: true })).toBeVisible();
  await check(select).toBeFocused();
  await page.keyboard.press('Escape');
  await check(page.locator('#chat-history-panel')).toBeHidden();
  await check(historyTrigger).toBeFocused();
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
    await draft.fill(retainedChatDraft);
    release();
    await check(send).toBeEnabled();
    await check(transcript).not.toContainText('Reply');
    await selectChat(page, '0');
    await check(historyTrigger).toContainText('Original conversation');
    await check(transcript).toContainText('Reply <img src=x onerror=alert(1)>');
    await check(transcript.locator('img,script')).toHaveCount(0);
    const heading = transcript.getByRole('heading', { name: 'Reply', exact: true });
    const table = transcript.getByRole('table');
    await check(heading).toHaveText('Reply');
    await check(table.getByRole('columnheader')).toHaveText(['Topic', 'Key']);
    await check(table.getByRole('cell')).toHaveText(['What', 'Virus']);
    await check(table.locator('strong')).toHaveText('What');
    await check(transcript.locator('code')).toHaveText('<img src=x onerror=alert(1)>');
    const guide = transcript.getByRole('link', { name: 'the guide', exact: true });
    await check(guide).toHaveAttribute('href', 'https://example.com/guide');
    await check(guide).toHaveAttribute('target', '_blank');
    await check(guide).toHaveAttribute('rel', 'noopener noreferrer');
    await check(transcript).not.toContainText('## Reply');
    await check(transcript).not.toContainText('| Topic |');
    await assertChatShell();
    await screenshot(page, 'ide-chat-rendered.png');
    await page.setViewportSize({ width: 390, height: 664 });
    await check(select).toHaveValue('0');
    await check(heading).toBeInViewport();
    await check(table).toBeInViewport();
    await check(table.getByRole('cell')).toHaveText(['What', 'Virus']);
    await assertChatShell();
    await screenshot(page, 'ide-chat-rendered-phone.png');
    await page.setViewportSize({ width: 375, height: 667 });
    await assertChatShell();
    await page.setViewportSize({ width: 844, height: 390 });
    await assertChatShell();
    await page.setViewportSize({ width: 1440, height: 1000 });
    await draft.fill('Follow-up');
    await send.click();
    await check(transcript.locator('[data-role=assistant]')).toHaveCount(2);
    assert.deepEqual(sent[1].map(message => message.content), [
      'Original conversation', replyMarkdown, 'Follow-up',
    ]);
    await page.setViewportSize({ width: 390, height: 664 });
    await assertChatShell();
    assert.equal(await transcript.evaluate(node => node.scrollHeight > node.clientHeight + 100), true,
      'a long reply scrolls within the open message stream');
    await check(draft).toBeInViewport();
    await check(send).toBeInViewport();
    await selectChat(page, '1');
    await check(draft).toHaveValue(retainedChatDraft);
    await draft.focus();
    await page.keyboard.press('Tab');
    await check(send).toBeFocused();
    await assertVisibleFocus(page);
    await page.setViewportSize({ width: 1440, height: 1000 });
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
  const { page, errors, respondToLeaveWarning } = await connect(context, fixture, false, async (page, withWarning) => {
    assert.equal(traffic.count('resume'), 1, 'startup resumes once');
    const forgeUrl = page.url();
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await check(page).toHaveURL(beforeForgeUrl);
    await page.goForward({ waitUntil: 'domcontentloaded' });
    await check(page.locator('#chat')).toBeVisible();
    assert.equal(traffic.count('resume'), 2, 'clean Forward reconnects once');
    const draft = page.locator('#chat-draft');
    await draft.click();
    await page.keyboard.type('Unsent Back draft');
    await check(page.locator('#send-chat')).toBeEnabled();
    await withWarning(() => page.evaluate(() => history.back()), 'stay');
    await check(page).toHaveURL(forgeUrl);
    await check(draft).toHaveValue('Unsent Back draft');
    await draft.fill('');
    await check(page.locator('#send-chat')).toBeDisabled();
  });
  assert.deepEqual(errors, [], 'clean Back does not prompt and native draft warning only occurs when expected');
  assert.equal(traffic.count('pair'), 1, 'one explicit pairing request');
  await projectOptionsJourney(page, traffic);
  await chatJourney(page, context);
  await inspect(page, 'workspace');
  await check(page.locator('#workspace-panel .fg-pane-body textarea:visible, #workbench [contenteditable="true"]:visible')).toHaveCount(0);
  await check(page.locator('#workbench-chat')).toBeHidden();
  await check(page.getByRole('button', { name: /^(Run|Debug|Terminal|Save file)$/i })).toHaveCount(0);
  await check(page.locator('#source-shape-panel')).toHaveJSProperty('open', false);
  await check(page.locator('#inspection-details')).toHaveJSProperty('open', false);
  await check(page.locator('#phase-label')).toBeHidden();
  await check(page.locator('#inspection-actions')).toBeHidden();
  await check(page.locator('#problems-panel')).toBeHidden();
  await disclosure(page, 'inspection-details', true);
  await check(page.locator('#phase-label')).toBeVisible();
  await check(page.locator('#phase-label')).toHaveText('Partial report');
  await assertTone(page, 'phase-label', 'warning');
  await check(page.locator('#summary')).toContainText('1 traits');
  await check(page.locator('#summary')).toContainText('2 trait impls');
  await check(page.locator('#summary')).toContainText('1 free functions');
  await disclosure(page, 'inspection-details', false);
  await check(page.locator('#phase-label')).toBeHidden();
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
  await reportAndPageLifecycle(page, traffic, respondToLeaveWarning);
  report.start('real.report.empty');

  const beforeSelection = traffic.count('inspect');
  await selectProject(page, 'empty');
  await check(page.locator('#workbench-context')).toHaveText('Local fixture / empty');
  assert.equal(traffic.count('inspect'), beforeSelection, 'selection changes the context before inspection');
  await disclosure(page, 'project-options', false);
  await page.locator('#inspect').click();
  await check(page.locator('#status')).toContainText('Project ready.');
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
  await check(page.locator('#inspection-actions')).toBeVisible();
  await check(page.locator('#reload-session')).toBeVisible();
  await check(page.locator('#cancel')).toBeHidden();
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
  const { page, errors, respondToLeaveWarning } = await connect(context, fixture);
  await selectProject(page, 'slow');
  await disclosure(page, 'project-options', false);
  const before = traffic.count('inspect');
  await page.locator('#inspect').evaluate(button => { button.click(); button.click(); button.click(); });
  await check(page.locator('#status')).toContainText('Inspecting');
  await check(page.locator('#inspect')).toBeDisabled();
  const actions = page.locator('#inspection-actions');
  await check(actions).toBeVisible();
  await check(page.locator('#reload-session')).toBeHidden();
  const cancel = page.locator('#cancel');
  await check(cancel).toBeEnabled();
  await check(cancel).toBeVisible();
  await cancel.focus();
  await check(cancel).toBeFocused();
  await page.keyboard.press('Enter');
  await check(page.locator('#status')).toContainText('Stopped waiting locally.');
  await assertTone(page, 'status', 'warning');
  await check(cancel).toBeHidden();
  await check(actions).toBeHidden();
  await check(page.locator('#inspect')).toBeFocused();
  await delay(1000); // injected native port completes after local cancellation
  await check(page.locator('#status')).toContainText('Stopped waiting locally.');
  await noReport(page);
  assert.equal(traffic.count('inspect'), before + 1, 'rapid repeated Inspect creates one request');
  await inspect(page, 'workspace', 'Project ready.');
  await check(page.locator('#files')).toContainText('fault-fixture.txt');
  await check(actions).toBeHidden();
  report.pass('injected.requests.cancel_retry');
  await controlledInputDraftJourney(page);
  report.start('injected.requests.revisions');

  await selectProject(page, 'slow');
  await page.locator('#inspect').click();
  await check(page.locator('#status')).toContainText('Inspecting');
  await selectProject(page, 'workspace');
  await selectProject(page, 'slow');
  await selectProject(page, 'workspace');
  await delay(1000);
  await check(page.locator('#status')).toContainText('Selection changed.');
  await noReport(page);
  report.pass('injected.requests.revisions');
  report.start('injected.requests.refresh_focus');

  const pendingBookmark = checkpointSaved(page, snapshot => snapshot.workbench_open && snapshot.workbench?.project === 'slow');
  await selectProject(page, 'slow');
  await pendingBookmark;
  await page.locator('#inspect').click();
  await check(page.locator('#status')).toContainText('Inspecting');
  const refreshRequests = traffic.count('inspect');
  const refreshResumes = traffic.count('resume');
  const refreshIdentities = traffic.uniqueInspections();
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await check(page.locator('#workbench')).toBeVisible();
  await check(page.locator('#status')).toContainText('Project ready.');
  await check(page.locator('#source-view')).toHaveText('');
  assert.equal(traffic.count('inspect'), refreshRequests + 1, 'refresh performs one fresh inspection of the bookmarked project');
  assert.equal(traffic.uniqueInspections(), refreshIdentities + 1, 'refresh does not replay the previous inspection identity');
  assert.equal(traffic.count('resume'), refreshResumes + 1);
  assert.equal(page.workers().length, 1);
  await selectProject(page, 'slow');
  await page.locator('#inspect').click();
  await check(page.locator('#status')).toContainText('Inspecting');
  await page.locator('#project').focus();
  await check(page.locator('#status')).toContainText('Project ready.');
  await check(page.locator('#project')).toBeFocused();
  report.pass('injected.requests.refresh_focus');
  report.start('injected.failures.unavailable_timeout');

  const recoveryDraft = 'Keep this draft through explicit reload';
  await page.locator('#open-chat').click();
  const reloadDraft = page.locator('#chat-draft');
  await reloadDraft.fill(recoveryDraft);
  await check(page.locator('#send-chat')).toBeEnabled();
  await page.locator('#open-workbench').click();
  const reloadBookmark = checkpointSaved(page, snapshot => snapshot.workbench_open &&
    snapshot.workbench?.project === 'unavailable' && checkpointHasDraft(snapshot, recoveryDraft));
  await inspect(page, 'unavailable', 'node observation unavailable');
  await noReport(page);
  await check(page.locator('#inspect')).toBeEnabled();
  const failedCount = traffic.count('inspect');
  await delay(250);
  assert.equal(traffic.count('inspect'), failedCount, 'unavailable does not auto-retry');
  await reloadBookmark;
  process.stderr.write('QA_CHECK explicit_reload_confirmation\n');
  const recovery = page.locator('#reload-session');
  const leaveDialog = page.locator('#leave-confirmation');
  const beforeRecovery = traffic.count('resume');
  await recovery.click();
  await check(leaveDialog).toBeVisible();
  await check(leaveDialog.getByRole('heading', { name: 'Leave this page?' })).toBeVisible();
  await check(leaveDialog).toContainText('Forge will reload and try to restore your workspace. Any work that hasn’t been saved may be lost.');
  const stay = leaveDialog.getByRole('button', { name: 'Stay', exact: true });
  await check(stay).toBeFocused();
  await stay.click();
  await check(leaveDialog).toBeHidden();
  await check(page.locator('#status')).toContainText('node observation unavailable');
  assert.equal(traffic.count('resume'), beforeRecovery, 'Stay does not reload the document');
  await recovery.click();
  await check(leaveDialog).toBeVisible();
  await Promise.all([
    page.waitForEvent('domcontentloaded'),
    leaveDialog.getByRole('button', { name: 'Leave', exact: true }).click(),
  ]);
  await check(page.locator('#workbench')).toBeVisible();
  await check(page.locator('#status')).toContainText('node observation unavailable');
  assert.equal(traffic.count('resume'), beforeRecovery + 1, 'Leave reloads once without a native double prompt');
  await page.locator('#open-chat').click();
  await check(reloadDraft).toHaveValue(recoveryDraft);
  await reloadDraft.fill('');
  await check(page.locator('#send-chat')).toBeDisabled();
  await page.locator('#open-workbench').click();
  await inspect(page, 'timeout', 'Deadline expired');
  await check(page.locator('#inspect')).toBeEnabled();
  await noReport(page);
  await delay(700); // allow non-preemptible injected port to finish
  await check(page.locator('#status')).toContainText('Deadline expired');
  await assertTone(page, 'status', 'error');
  await screenshot(page, 'ide-controlled-timeout.png');
  report.pass('injected.failures.unavailable_timeout');
  report.start('probe.worker.fatal');

  const fatalBookmark = checkpointSaved(page, snapshot => snapshot.workbench_open && snapshot.workbench?.project === 'workspace');
  await inspect(page, 'workspace', 'Project ready.');
  await fatalBookmark;
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
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await check(page.locator('#status')).toContainText('Project ready.');
  await check(page.locator('#source-view')).toHaveText('');
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
  await page.locator('#workbench-context').click();
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await check(page.locator('#status')).toContainText('Connected.');
  await check(page.locator('#workbench')).toBeVisible();
  await check(page.locator('#workbench')).toContainText(/No allowed project/i);
  await check(page.locator('#inspect')).toBeDisabled();
  await check(page.locator('#project option')).toHaveCount(0);
  await check(page.locator('#phase-label')).toBeHidden();
  await disclosure(page, 'inspection-details', true);
  await check(page.locator('#phase-label')).toHaveText('No allowed project');
  await assertTone(page, 'phase-label', 'warning');
  await disclosure(page, 'inspection-details', false);
  await noReport(page);
  await screenshot(page, 'ide-controlled-no-allowed-project.png');
  await context.unroute('**/forge/v1/dispatch');
  report.pass('injected.catalog.empty');
  report.start('probe.session.missing');

  await context.clearCookies();
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
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
    // user profile. Native durable Chat uses the test relay on loopback.
    browserOwner = acquireOwned(
      () => chromium.launch({ headless: true, chromiumSandbox: true, channel }),
      ownedBrowser => ownedBrowser.close(),
    );
    browser = await browserOwner.ready;
    report.browserVersion(browser.version());
    report.ensureActive();
  },
  async journey() {
    try {
      await realJourney();
      await controlledLifecycleJourney();
    } catch (error) {
      const location = String(error.stack).match(/journey\.mjs:(\d+):(\d+)/);
      if (location) process.stderr.write(`QA_CHECK journey_location=${location[1]}:${location[2]}\n`);
      throw error;
    }
  },
  diagnose: (currentReport, directory) => captureFailureScreenshot(currentReport, directory, [...active], process.env.FORGE_UI_QA_FAILURE_SCREENSHOT === '1'),
  async cleanup(currentReport) {
    await cleanupResources([
      ...(browserOwner ? [{ name: 'browser', close: () => browserOwner.close() }] : []),
      ...[...active].map(fixture => ({ name: fixture.name, close: () => stopFixture(fixture) })),
      ...(durableRelayOwner ? [{ name: 'real_fixture', close: () => durableRelayOwner.close() }] : []),
    ], currentReport);
  },
});
if (!passed) process.exitCode = 1;
