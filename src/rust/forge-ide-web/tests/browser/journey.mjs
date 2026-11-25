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
import { startDurableRelay } from './durable-relay.mjs';
import { startAgentsRelay } from './agents-relay.mjs';

let assets, agent, fixtureBinary, output, browser, browserOwner, durableRelayOwner, agentsRelayOwner, check, report;
const active = new Set();
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function startFixture(faults = false, singleProject = false, { durable, agents, managedWrites = false } = {}) {
  report.ensureActive();
  report.setStage('fixture_start');
  const mode = durable ? ['--durable', durable.bind, durable.endpoint, durable.tokenFile]
    : agents ? ['--agents', '127.0.0.1:0', agents.endpoint, agents.tokenFile]
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
const localActions = ['select_entry', 'toggle_entry', 'select_declaration', 'select_problem', 'set_explorer_query', 'set_symbol_filter', 'more_tree', 'previous_tree', 'more_outline', 'previous_outline', 'more_problems', 'previous_problems'];

// Test-only observation: never replace the Worker implementation, WASM or gateway.
// Retain only local report actions in ephemeral page memory, never pairing events,
// request headers, cookies or network bodies. Nothing in this probe is an artifact.
async function observe(context) {
  const counts = new Map();
  const inspectionIds = new Set();
  const business = { mail: 0, work: 0, usage: 0, chat: 0, proposal: 0 };
  let catalogMailEnabled = null;
  context.on('request', request => {
    if (new URL(request.url()).pathname !== '/forge/v1/dispatch') return;
    let kind;
    try { kind = request.postDataJSON()?.kind; } catch { kind = 'invalid'; }
    if (typeof kind === 'string') {
      for (const prefix of ['mail', 'work', 'usage']) if (kind.startsWith(prefix + '_')) business[prefix]++;
      if (kind === 'chat' || kind.startsWith('chat_')) business.chat++;
      if (kind === 'propose_edit') business.proposal++;
    }
    if (!['resume', 'pair', 'inspect', 'source', 'chat_history_retention_set', 'chat_run_submit'].includes(kind)) kind = 'other';
    counts.set(kind, (counts.get(kind) || 0) + 1);
    if (kind === 'inspect') inspectionIds.add(request.headers()['forge-request-id']);
  });
  context.on('response', async response => {
    // Keep only this fixture capability bit, never its token/catalog body.
    if (!response.ok() || new URL(response.url()).pathname !== '/forge/v1/dispatch') return;
    try {
      if (!['resume', 'pair'].includes(response.request().postDataJSON()?.kind)) return;
      const catalog = (await response.json()).event?.catalog;
      // Catalog serializes a false capability by omitting its default value.
      if (catalog && typeof catalog === 'object') catalogMailEnabled = catalog.mail_enabled === true;
    } catch { /* The real application owns response validation. */ }
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
        if ((kind.endsWith('_filter') || kind === 'set_explorer_query') && (typeof action.value !== 'string' || new TextEncoder().encode(action.value).length > 256)) {
          throw new Error('Filter event omitted its bounded query value');
        }
        if (kind === 'set_explorer_query' && typeof action.entry_names !== 'boolean') {
          throw new Error('Explorer query omitted its accepted mode');
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
  return { count: kind => counts.get(kind) || 0, uniqueInspections: () => inspectionIds.size,
    businessCalls: () => ({ ...business }), catalogMailEnabled: () => catalogMailEnabled };
}
const beforeForgeUrl = 'data:text/html,<!doctype html><title>Forge QA previous page</title>';
const retiredChatDraft = 'Keep this dormant Chat draft for a future return';

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
  await check(page.locator('#agents')).toBeVisible();
  await assertRetiredChat(page);
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
async function setExplorerMode(page, entryNames) {
  const toggle = page.locator('#explorer-filter-mode');
  await check(toggle).toBeEnabled();
  if (await toggle.getAttribute('aria-pressed') !== String(entryNames)) await toggle.click();
  await check(toggle).toHaveAttribute('aria-pressed', String(entryNames));
}
async function searchProject(page, query) {
  await setExplorerMode(page, false);
  await page.locator('#project-query').fill(query);
}
async function filterFiles(page, query) {
  await setExplorerMode(page, true);
  await page.locator('#project-query').fill(query);
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
async function screenshot(page, name, { home = false, docs = false } = {}) {
  assert.equal(await screenshotEligibility(page, [...active], { home, docs }), 'safe', 'Only sanitized owned fixture pages may be captured');
  if (!home && !docs) await check(page.locator('#pair-code')).toHaveValue('');
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
  await sheet(page, 'project-options', true);
  await page.locator('#project').selectOption(project);
  await sheet(page, 'project-options', false);
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

async function sheet(page, id, open) {
  const dialog = page.locator('#' + id);
  const trigger = page.locator(id === 'project-options' ? '#workbench-context' : '#workspace-options-toggle');
  if (await dialog.evaluate(node => node.open) !== open) {
    if (open) {
      for (const other of ['project-options', 'workspace-options'].filter(name => name !== id)) {
        if (await page.locator('#' + other).evaluate(node => node.open)) {
          await page.locator('#' + other + '-close').click();
          await check(page.locator('#' + other)).toHaveJSProperty('open', false);
        }
      }
      await check(trigger).toBeVisible();
      await trigger.focus();
      await page.keyboard.press('Enter');
    } else {
      await page.locator('#' + id + '-close').click();
    }
  }
  await check(dialog).toHaveJSProperty('open', open);
  if (open) await check(dialog).toBeVisible();
  else await check(dialog).toBeHidden();
}
async function disclosure(page, id, open) {
  if (['project-options', 'workspace-options'].includes(id)) return sheet(page, id, open);
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
      const controls = ['chat-draft', 'chat-model', 'send-chat', 'open-chat', 'open-agents',
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
async function assertRetiredChat(page, traffic) {
  await check(page.locator('#chat,#open-chat,#chat-header-actions,#workbench-chat,#send-chat,#propose-workbench-edit')).toHaveCount(0);
  if (traffic) {
    assert.equal(traffic.businessCalls().chat, 0, 'retired Chat emits no inference, history, model or run request');
    assert.equal(traffic.businessCalls().proposal, 0, 'retired Chat cannot be reached through a file proposal');
  }
}
function dormantChat() {
  return {
    selected: 1, model_id: 'qwen2.5-coder-7b',
    conversations: [
      { chat_id: '11111111-1111-4111-8111-111111111111', revision: 3,
        messages: [{ role: 'user', content: 'A retained saved question' }, { role: 'assistant', content: 'A retained saved answer' }], draft: 'First dormant draft' },
      { chat_id: '22222222-2222-4222-8222-222222222222', revision: 4,
        messages: [], draft: retiredChatDraft },
    ],
    pending_run: { chat_id: '22222222-2222-4222-8222-222222222222',
      idempotency_key: '33333333-3333-4333-8333-333333333333',
      run_id: '44444444-4444-4444-8444-444444444444', expected_revision: 4,
      model_id: 'qwen2.5-coder-7b' },
  };
}
async function retiredEvent(page, event) {
  const before = await page.evaluate(() => window.__forgeBrowserProbe.receivedViews());
  await page.workers()[0].evaluate(event => self.onmessage({ data: event }), event);
  await check.poll(() => page.evaluate(() => window.__forgeBrowserProbe.receivedViews())).toBeGreaterThan(before);
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
  await page.mouse.move(0, 0);
  assert.equal(await refresh.evaluate(button => {
    const reference = document.createElement('span');
    document.body.append(reference);
    const color = token => {
      reference.style.backgroundColor = `var(--${token})`;
      return getComputedStyle(reference).backgroundColor;
    };
    const background = getComputedStyle(button).backgroundColor;
    const neutral = ['rgba(0, 0, 0, 0)', color('bg'), color('surface'), color('raised')].includes(background);
    const accent = color('accent');
    reference.remove();
    return neutral && background !== accent;
  }), true, 'refresh uses the neutral canvas or a transparent surface');
  const restingBackground = await refresh.evaluate(button => getComputedStyle(button).backgroundColor);
  await refresh.hover();
  await check.poll(() => refresh.evaluate(button => getComputedStyle(button).backgroundColor)).not.toBe(restingBackground);
  await page.mouse.move(0, 0);
  const [problemsBounds, refreshBounds] = await Promise.all([problems.boundingBox(), refresh.boundingBox()]);
  assert(problemsBounds && refreshBounds && problemsBounds.y < refreshBounds.y + refreshBounds.height &&
    refreshBounds.y < problemsBounds.y + problemsBounds.height, 'Problems and Refresh project share a row');
  if (page.viewportSize().width <= 390) {
    assert(Math.abs(refreshBounds.width - 44) < 1 && Math.abs(refreshBounds.height - 44) < 1,
      'phone refresh target is 44 by 44 CSS pixels');
  }
  const gap = refreshBounds.x - (problemsBounds.x + problemsBounds.width);
  assert(gap >= -1 && refreshBounds.x + refreshBounds.width <= page.viewportSize().width + 1,
    'Refresh project follows Problems at the toolbar trailing edge');
  if (page.viewportSize().width <= 800) assert(gap <= 12, 'phone Refresh remains adjacent to the pane tabs');
  await problems.focus();
  await page.keyboard.press('Tab');
  await check(refresh).toBeFocused();
  assert.equal(await refresh.evaluate(button => {
    const style = getComputedStyle(button);
    return button.matches(':focus-visible') && style.outlineStyle === 'solid' && Number.parseFloat(style.outlineWidth) >= 2;
  }), true, 'refresh retains a clear keyboard focus ring');
  await refresh.click({ trial: true });
}
async function projectOptionsJourney(page, traffic) {
  await sheet(page, 'workspace-options', false);
  await sheet(page, 'project-options', false);
  await checkRefreshPlacement(page);
  const projectDialog = page.getByRole('dialog', { name: 'Project options', exact: true });
  const viewDialog = page.getByRole('dialog', { name: 'View options', exact: true });
  const projectTrigger = page.getByRole('button', { name: 'Project options', exact: true });
  const viewTrigger = page.getByRole('button', { name: 'View options', exact: true });
  await check(projectTrigger).toHaveAttribute('id', 'workbench-context');
  await check(projectTrigger).toBeInViewport();
  await check(viewTrigger).toBeInViewport();
  const [projectBounds, viewBounds] = await Promise.all([projectTrigger.boundingBox(), viewTrigger.boundingBox()]);
  assert(projectBounds && viewBounds && projectBounds.y < viewBounds.y + viewBounds.height &&
    viewBounds.y < projectBounds.y + projectBounds.height, 'project breadcrumb and view options share the identity header');
  const requests = ['resume', 'pair', 'inspect', 'source', 'other'];
  const before = requests.map(traffic.count);
  await sheet(page, 'workspace-options', true);
  await check(viewDialog).toBeVisible();
  await check(page.locator('#explorer-panel-toggle')).toBeFocused();
  await check(projectDialog).toBeHidden();
  await page.keyboard.press('Escape');
  await check(viewDialog).toBeHidden();
  await check(viewTrigger).toBeFocused();
  await sheet(page, 'project-options', true);
  await check(projectDialog).toBeVisible();
  await check(viewDialog).toBeHidden();
  await check(page.locator('#node')).toBeFocused();
  await page.keyboard.press('Tab');
  await check(page.locator('#project')).toBeFocused();
  assert.equal(await page.locator('#project-options select').evaluateAll(fields =>
    fields.length === 2 && fields.every(field => field.getBoundingClientRect().width >= 120 && field.getBoundingClientRect().height >= 44)),
  true, 'sheet selectors retain readable widths and touch targets');
  if (page.viewportSize().width < 640) {
    const bounds = await projectDialog.boundingBox();
    const viewport = page.viewportSize();
    assert(bounds && bounds.x >= -1 && bounds.x + bounds.width <= viewport.width + 1
      && Math.abs(bounds.y + bounds.height - viewport.height) <= 2,
    'project options use a viewport-width bottom sheet on phones');
    await screenshot(page, 'ide-workbench-project-options-phone.png');
  }
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
    'open project sheet does not overflow the viewport');
  await page.keyboard.press('Escape');
  await check(projectDialog).toBeHidden();
  await check(projectTrigger).toBeFocused();
  await check(page.locator('#node')).toBeHidden();
  await check(page.locator('#project')).toBeHidden();
  await sheet(page, 'workspace-options', true);
  await check(viewDialog).toBeVisible();
  if (page.viewportSize().width < 640) {
    const bounds = await viewDialog.boundingBox();
    assert(bounds && Math.abs(bounds.y + bounds.height - page.viewportSize().height) <= 2,
      'view options use a phone bottom sheet');
    await screenshot(page, 'ide-workbench-options-phone.png');
  }
  await sheet(page, 'workspace-options', false);
  await check(viewTrigger).toBeFocused();
  await sheet(page, 'project-options', true);
  await page.evaluate(() => {
    document.getElementById('project-options').close();
    document.getElementById('inspect').focus({ preventScroll: true });
  });
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await check(projectDialog).toBeHidden();
  await check(projectTrigger).toHaveAttribute('aria-expanded', 'false');
  await check(page.locator('#inspect')).toBeFocused();
  assert.deepEqual(requests.map(traffic.count), before, 'project and view sheets remain presentation-only');
}

async function checkWorkbenchChrome(page) {
  const viewport = page.viewportSize();
  await check(page.locator('#workbench')).toBeVisible();
  await check(page.locator('#file-filter')).toHaveCount(0);
  const mode = page.locator('#explorer-filter-mode');
  await check(mode).toHaveAttribute('aria-label', 'Filter entry names');
  await check(page.locator('#project-query')).toHaveAttribute('placeholder', 'Search files or symbols…');
  if (viewport.width < 640) {
    const [identity, globalTabs, paneTabs] = await Promise.all([
      page.locator('.fg-header').boundingBox(), page.locator('#ide-navigation').boundingBox(),
      page.locator('#workspace .fg-pane-actions').boundingBox(),
    ]);
    assert(identity && identity.height <= 56 && globalTabs && globalTabs.height <= 48
      && paneTabs && paneTabs.height <= 48,
    'mobile Workbench has a compact identity header and two control/tab strips');
    await check(page.locator('#workspace .fg-layout-options')).toHaveCount(0);
    for (const pane of ['explorer-panel', 'workspace-panel', 'outline-panel', 'problems-panel']) {
      await check(page.locator('#' + pane + ' > .fg-pane-header')).toBeHidden();
    }
    await check(page.locator('#workspace .fg-pane-close:visible')).toHaveCount(0);
    for (const id of ['workbench-context', 'workspace-options-toggle', 'explorer-panel-switch', 'outline-panel-switch', 'problems-panel-switch', 'inspect']) {
      const box = await page.locator('#' + id).boundingBox();
      assert(box && box.width >= 44 && box.height >= 44, 'mobile Workbench controls retain touch targets');
    }
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'compact Workbench has no horizontal page overflow');
  }
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
  await page.locator('#project-query').pressSequentially('zz-pagination-matchX');
  await page.locator('#project-query').press('Backspace');
  await check(page.locator('#project-query')).toHaveValue('zz-pagination-match');
  await check(page.locator('#files')).toContainText('bulk');
  await check(page.locator('#files')).toContainText('zz-pagination-match.txt');
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  await check(page.locator('#explorer-summary')).toContainText(/hidden by filter/i);
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
  const filterEvents = await page.evaluate(() => window.__forgeBrowserProbe.count('set_explorer_query'));
  await page.locator('#project-query').fill('é'.repeat(129));
  assert.equal(await page.locator('#project-query').evaluate(input => input.validity.valid), false, 'UTF-8 byte bound is enforced, not merely character count');
  assert.equal(await page.evaluate(() => window.__forgeBrowserProbe.count('set_explorer_query')), filterEvents, 'oversized filter never reaches the worker');
  await filterFiles(page, 'NO-MATCH');
  await check(page.locator('#files [role="treeitem"]')).toHaveCount(0);
  await check(page.locator('#files')).toContainText(/match/i);
  await filterFiles(page, '');
  await check(fileRow(page, 'blank.rs')).toBeVisible();
  await selectFile(page, 'blank.rs');
  await check(page.locator('#outline')).toHaveText('No symbols in this file.');
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
    await sheet(page, 'workspace-options', true);
    await page.locator('#workspace-reset').click();
    await sheet(page, 'workspace-options', false);
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
    await sheet(page, 'workspace-options', true);
    await page.locator('#workspace-reset').click();
    await sheet(page, 'workspace-options', false);
    await ensurePane(page, pane);
    await check(splitter).toHaveAttribute('aria-valuenow', String(initial));
    await splitter.focus();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeHidden();
    await check(page.locator(`#${pane}-toggle`)).toBeFocused();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeVisible();
    await assertVisibleFocus(page);
    await sheet(page, 'workspace-options', false);
    await page.locator(`#${pane}-close`).focus();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}-toggle`)).toBeFocused();
    await page.keyboard.press('Enter');
    await check(page.locator(`#${pane}`)).toBeVisible();
    await sheet(page, 'workspace-options', false);
  }
  await sheet(page, 'workspace-options', false);
  await check(page.locator('#selection-title')).toContainText('lib.rs');
  report.pass('real.panes.desktop');
  report.start('real.panes.narrow');

  // A resize preserves the foreground pane's focus. The preceding desktop
  // exercise ends in Problems management, so establish the intended selected
  // file as foreground before checking its narrow presentation.
  await page.locator('#selection-title').focus();
  await check(page.locator('#selection-title')).toBeFocused();
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
  await saveAction(page, 'set_explorer_query', 'old-report-filter');
  await inspect(page, 'workspace');
  await noSelectedFileFromPriorReport(page);
  await replayAction(page, 'old-report-row');
  await replayAction(page, 'old-report-filter');
  await check(page.locator('#project-query')).toHaveValue('');
  await noSelectedFileFromPriorReport(page);
  const rows = await page.locator('#files [role="treeitem"]').count();
  const target = fileRow(page, 'README <safe>.md');
  const saved = checkpointSaved(page, snapshot => snapshot.workbench_open &&
    snapshot.workbench?.entry?.path.at(-1)?.value === 'README <safe>.md');
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
  await sheet(page, 'workspace-options', true);
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
      contextHidden: document.getElementById('workbench-context').closest('[hidden]') !== null,
      contextText: document.getElementById('workbench-context').textContent,
      sheetsClosed: ['project-options', 'workspace-options'].every(id => !document.getElementById(id).open),
      source: document.getElementById('source-view').textContent,
      markdown: document.getElementById('source-preview').textContent,
      retiredChatAbsent: document.querySelectorAll('#chat,#open-chat,#chat-draft').length === 0,
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
  assert.equal(cleared.contextText.includes('Local fixture'), false, 'teardown clears the projected project context before resume');
  assert.equal(cleared.sheetsClosed, true, 'teardown closes both presentation sheets before resume');
  assert.equal(cleared.source, '', 'teardown clears verified source before resume');
  assert.equal(cleared.markdown, '', 'teardown clears displayed Markdown before resume');
  assert.equal(cleared.retiredChatAbsent, true, 'retired Chat has no protected DOM during teardown');
  await page.evaluate(() => {
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
  });
  await checkRestoredWorkbench(before);
  process.stderr.write('QA_CHECK native_back_warning\n');
  await page.locator('.fg-header').click({ position: { x: 2, y: 2 } });
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
  await page.locator('.fg-header').click({ position: { x: 2, y: 2 } });
  before = requestCounts();
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await checkRestoredWorkbench(before);
  await assertRetiredChat(page, traffic);
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
    await check(otherTab.locator('#agents')).toBeVisible();
    await assertRetiredChat(otherTab, traffic);
    await noReport(otherTab);
    await otherTab.locator('#open-workbench').click();
    const saved = checkpointSaved(otherTab, snapshot => snapshot.workbench_open &&
      snapshot.workbench?.entry?.path.at(-1)?.value === 'README <safe>.md');
    await inspect(otherTab, 'workspace');
    await fileRow(otherTab, 'README <safe>.md').click();
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
    await check(otherTab.locator('#workbench')).toBeVisible();
    await check(otherTab.locator('#selection-title')).toContainText('README <safe>.md');
    await assertRetiredChat(otherTab, traffic);
  } finally {
    await otherTab.close();
  }
  await durableHostRestartJourney();
  report.pass('probe.page.lifecycle');
}

const portalViewNames = ['Agents', 'Workbench', 'Home', 'Design'];

function legacyMailNavigation() {
  return { active: true, archived: false, composing: true, thread_id: null,
    subject: 'Retained legacy Mail subject', draft: 'Retained legacy Mail draft, never replayed.' };
}

async function assertNoRetiredMail(page) {
  await check(page.locator('#mail,#open-mail')).toHaveCount(0);
}

async function assertApplicationViews(page, active) {
  await assertNoRetiredMail(page);
  const navigation = page.locator('#ide-navigation');
  await check(navigation.locator(':scope > :is(button,a)')).toHaveText(portalViewNames);
  await check(page.locator('#open-agents')).toBeVisible();
  if (active === 'Agents') {
    await check(page.locator('#open-agents')).toHaveAttribute('aria-current', 'page');
    await check(page.locator('#open-agents')).toHaveAttribute('aria-pressed', 'true');
    await check(navigation.locator('[aria-current="page"]')).toHaveCount(1);
  } else {
    await check(page.locator('#open-agents')).not.toHaveAttribute('aria-current', 'page');
  }
}

async function assertNativeViews(page, active) {
  if (active === 'Home') {
    // Home is the launcher rather than a workspace; its header intentionally
    // has no duplicate IDE tab strip.
    await check(page.locator('.home-launcher-agents')).toHaveAttribute('href', '/agents');
    await check(page.locator('a[href="/#mail"],a[href="/mail"]')).toHaveCount(0);
    return;
  }
  const views = page.getByRole('navigation', { name: 'Forge views' });
  await check(views.getByRole('link')).toHaveText(portalViewNames);
  await check(views.getByRole('link', { name: active, exact: true })).toHaveAttribute('aria-current', 'page');
  for (const [name, href] of [['Agents', '/#agents'], ['Workbench', '/#workbench'], ['Home', '/home'], ['Design', '/design']]) {
    const link = views.getByRole('link', { name, exact: true });
    await check(link).toHaveAttribute('href', href);
    await check(link).not.toHaveAttribute('target', /.+/);
  }
  await check(views.locator('a[href="/#mail"]')).toHaveCount(0);
}

async function unconfiguredAgentsNavigationJourney(page, desktop, traffic, origin) {
  process.stderr.write('QA_CHECK unconfigured_agents_navigation\n');
  await check.poll(traffic.catalogMailEnabled).toBe(false);
  assert.deepEqual(traffic.businessCalls(), { mail: 0, work: 0, usage: 0, chat: 0, proposal: 0 });
  const pages = page.context().pages();
  for (const width of [320, 375, 390]) {
    await page.setViewportSize({ width, height: 844 });
    await check(page.locator('#agents')).toBeVisible();
    await assertApplicationViews(page, 'Agents');
    await assertRetiredChat(page, traffic);
    await check(page.locator('#agents-unsupported')).toContainText('not configured');
    await check(page.locator('#agents-new')).toBeHidden();
    const tab = await page.locator('#open-agents').boundingBox();
    assert(tab && tab.width >= 44 && tab.height >= 44, 'Agents keeps a mobile touch target');
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'the four-view navigation fits unconfigured phone viewports');
  }
  await page.locator('#forge-home').click();
  await check(page).toHaveURL(origin + '/home');
  await assertNativeViews(page, 'Home');
  const retiredMail = legacyMailNavigation();
  await page.evaluate(mail => {
    const key = 'forge-workspace-navigation';
    const cache = JSON.parse(sessionStorage.getItem(key));
    if (!cache?.snapshot) throw new Error('Expected genuine workspace handoff');
    cache.mail = mail;
    sessionStorage.setItem(key, JSON.stringify(cache));
  }, retiredMail);
  const legacyRedirect = await page.request.get(origin + '/mail', { maxRedirects: 0 });
  assert.equal(legacyRedirect.status(), 302);
  assert.equal(legacyRedirect.headers().location, '/#agents');
  await legacyRedirect.dispose();
  for (const fragment of ['mail', 'chat']) {
    await page.goto(origin + '/#' + fragment, { waitUntil: 'domcontentloaded' });
    await check(page.locator('#agents')).toBeVisible();
    await check(page).toHaveURL(origin + '/');
    await assertApplicationViews(page, 'Agents');
    await assertRetiredChat(page, traffic);
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(origin + '/home');
  }
  const inertMail = await page.evaluate(() => JSON.parse(sessionStorage.getItem('forge-workspace-navigation'))?.mail);
  assert.deepEqual(inertMail, { ...retiredMail, active: false });
  await page.locator('.home-launcher-ide').click();
  await check(page.locator('#agents')).toBeVisible();
  assert.deepEqual(traffic.businessCalls(), { mail: 0, work: 0, usage: 0, chat: 0, proposal: 0 });
  assert.deepEqual(page.context().pages(), pages);
  await page.setViewportSize(desktop);
}

async function durableHostRestartJourney() {
  const viewport = { width: 1440, height: 1000 };
  const context = await browser.newContext({ viewport });
  let fixture;
  durableRelayOwner = acquireOwned(() => startDurableRelay('', ''), relay => relay.close());
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
      if (previous) assert.equal(fixture.origin, previous.origin);
    }
    await restart();
    const traffic = await observe(context);
    const page = await context.newPage();
    fixture.page = page;
    await page.goto(fixture.origin, { waitUntil: 'domcontentloaded' });
    await page.locator('#pair-code').fill(fixture.code);
    await page.locator('#pair').click();
    fixture.code = '';
    await check(page.locator('#agents')).toBeVisible();
    const tab = await page.evaluate(() => sessionStorage.getItem('forge-workspace-tab'));
    const chat = dormantChat();
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin + '/home');
    relay.seedWorkspace(tab, { chat, workbench: null, workbench_open: false });
    await page.locator('.home-launcher-ide').click();
    await check(page.locator('#agents')).toBeVisible();
    await assertRetiredChat(page, traffic);
    await unconfiguredAgentsNavigationJourney(page, viewport, traffic, fixture.origin);
    for (const event of [
      {kind:'open_chat'}, {kind:'show_chat_history'}, {kind:'retry_chat_submit'},
      {kind:'cancel_chat_run'}, {kind:'send_chat'},
      {kind:'set_chat_draft',conversation:1,value:'Must not overwrite the dormant draft'},
    ]) await retiredEvent(page,event);
    await check(page.locator('#agents')).toBeVisible();
    await assertRetiredChat(page, traffic);
    await page.locator('#open-workbench').click();
    const saved = checkpointSaved(page, snapshot => snapshot.workbench_open &&
      snapshot.workbench?.entry?.path.at(-1)?.value === 'README <safe>.md');
    await inspect(page, 'workspace');
    await fileRow(page, 'README <safe>.md').click();
    await saved;
    assert.deepEqual(relay.workspaceSnapshot(tab).chat, chat,
      'workspace save preserves every dormant draft, message, model and pending-run identity');
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin + '/home');
    assert.deepEqual(relay.workspaceSnapshot(tab).chat, chat);
    await restart();
    fixture.page = page;
    await page.locator('.home-launcher-ide').click();
    // A host restart needs its new session pairing, never the old admission.
    await check(page.locator('#pair')).toBeEnabled();
    await noReport(page);
    await page.locator('#pair-code').fill(fixture.code);
    await page.locator('#pair').click();
    fixture.code = '';
    await check(page.locator('#workbench')).toBeVisible();
    await check(page.locator('#selection-title')).toContainText('README <safe>.md');
    await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
    await page.locator('#source-display').click();
    await page.locator('#source-raw').click();
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin + '/home');
    // Successful navigation acknowledges the current saved checkpoint. Pure
    // presentation changes need not create another identical save request.
    assert.equal(relay.workspaceSnapshot(tab).workbench_open, true);
    assert.equal(relay.workspaceSnapshot(tab).workbench.entry.path.at(-1).value, 'README <safe>.md');
    assert.deepEqual(relay.workspaceSnapshot(tab).chat, chat);
    assert.equal(relay.submitted(), 0, 'a dormant saved run is never submitted again');
    assert.equal(relay.completions(), 0, 'retirement does not manufacture a completion');
    assert.ok(relay.workspaceRestores().length >= 3);
    assert.ok(relay.workspaceRestores().every(request => request.preserve_chat && !request.history && !request.models),
      'all workspace restores preserve dormant Chat without history/model hydration');
    await assertRetiredChat(page, traffic);
    const fresh = await context.newPage();
    try {
      await fresh.goto(fixture.origin, { waitUntil: 'domcontentloaded' });
      await check(fresh.locator('#agents')).toBeVisible();
      await assertRetiredChat(fresh, traffic);
      await noReport(fresh);
    } finally { await fresh.close(); }
  } finally {
    await context.close();
    try { if (fixture) await stopFixture(fixture); }
    finally { await durableRelayOwner.close(); }
  }
}

async function noSelectedFileFromPriorReport(page) {
  await check(page.locator('#selection-title')).not.toContainText(LONG_FILE_NAME);
  await check(page.locator('#details')).not.toContainText(LONG_FILE_NAME);
}

async function checkPhoneFileBack(page) {
  const header = page.locator('#file-heading');
  const heading = page.locator('#selection-title');
  const back = page.getByRole('button', { name: 'Back to files', exact: true });
  await check(back).toHaveAttribute('id', 'back-to-files');
  await check(back).toHaveAttribute('aria-label', 'Back to files');
  await check(back).toHaveAttribute('title', 'Back to files');
  await check(back).not.toContainText('Back to files');
  await check(back).toBeInViewport();
  const [icon, title] = await Promise.all([back.boundingBox(), heading.boundingBox()]);
  assert(icon && title && Math.abs(icon.width - 44) < 1 && Math.abs(icon.height - 44) < 1,
    'the phone Back icon has a 44 by 44 CSS pixel target');
  assert(icon.x + icon.width <= title.x + 1 && icon.y < title.y + title.height && title.y < icon.y + icon.height,
    'Back to files shares the selected filename row');
  await check(header).toBeVisible();
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
  const origin = new URL(page.url()).origin;
  const diagramSrc = `${origin}/design/portal-navigation-sequence.svg`;
  const lines = [
    '## Fixture overview', '', '**Readable detail**', '', 'Fixture <script> text is never HTML.', '',
    '![Fixture image alternative](/fixture-diagram.png)', '',
    `![Fixture diagram](${diagramSrc})`,
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
  await check(preview.locator('script,svg')).toHaveCount(0);
  const diagram = preview.locator('img');
  await check(diagram).toHaveCount(1);
  await check(diagram).toHaveAttribute('src', diagramSrc);
  await check(diagram).toHaveAttribute('alt', 'Fixture diagram');
  await check.poll(() => diagram.evaluate(img => img.naturalWidth)).toBeGreaterThan(0);
  assert.equal(await diagram.evaluate(img => img.outerHTML),
    `<img src="${diagramSrc}" alt="Fixture diagram">`,
    'the rendered image carries only src and alt');
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
    await checkWorkbenchChrome(page);
    await check(page.locator('#workspace-panel-switch')).toBeDisabled();
    await screenshot(page, 'ide-workbench-explorer-phone.png');
    process.stderr.write('QA_CHECK source_search_connected\n');
    assert.equal(traffic.count('inspect'), 1, 'one admitted project opens automatically');
    await check(page.locator('#explorer-panel')).toBeVisible();
    process.stderr.write('QA_CHECK source_search_explorer\n');
    const reads = traffic.count('source');
    await searchProject(page, 'INSPECT_ME');
    const match = page.locator('#search-results [role="treeitem"]').filter({ hasText: 'inspect_me' });
    await check(match).toHaveCount(1);
    process.stderr.write('QA_CHECK source_search_match\n');
    await saveAction(page, 'set_explorer_query', 'old-explorer-mode');
    await setExplorerMode(page, true);
    await filterFiles(page, 'lib.rs');
    await check(fileRow(page, 'lib.rs')).toBeVisible();
    await replayAction(page, 'old-explorer-mode');
    await check(page.locator('#explorer-filter-mode')).toHaveAttribute('aria-pressed', 'true');
    await check(page.locator('#project-query')).toHaveValue('lib.rs');
    await searchProject(page, 'INSPECT_ME');
    await check(match).toHaveCount(1);
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
    await assertRetiredChat(page, traffic);
    await checkPhoneFileBack(page);
    await searchProject(page, 'README <safe>');
    await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'README <safe>.md' }).click();
    await check(page.locator('#workspace-panel')).toBeVisible();
    await check(page.locator('#source-view')).toContainText('Fixture <script> text is never HTML.');
    await check(page.locator('#source-view script, #source-view img, #source-view svg')).toHaveCount(0);
    await checkMarkdownSource(page, traffic);
    await page.getByRole('button', { name: 'Back to files', exact: true }).click();
    await check(page.locator('#explorer-panel')).toBeVisible();
    await searchProject(page, 'paged.txt');
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
    await searchProject(page, 'late_view');
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
    await homeJourney(page, traffic);
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    const beforeDesign = ['resume', 'inspect', 'source'].map(traffic.count);
    await designJourney(page, traffic);
    await check(page.locator('#source-view [data-line="301"] .fg-source-text')).toHaveText('    42');
    assert.deepEqual(['resume', 'inspect', 'source'].map(traffic.count), beforeDesign.map(count => count + 1),
      'returning from Design authenticates and re-verifies the bookmarked source');
    await assertRetiredChat(page, traffic);
    if (managedWrites) {
      await page.setViewportSize({ width: 390, height: 844 });
      await managedWriteJourney(page, context, respondToLeaveWarning);
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
  await searchProject(page, 'managed.txt');
  await page.locator('#search-results [role="treeitem"]').filter({ hasText: 'managed.txt' }).click();
  const source = page.locator('#source-view');
  await check(source).toContainText('old line');
  await assertRetiredChat(page);
  await page.getByRole('button', { name: 'Edit file', exact: true }).click();
  const editor = page.getByRole('textbox', { name: 'File contents' });
  const original = await editor.inputValue();
  const edited = original.replace('old line', 'saved with direct editor');
  await editor.fill(edited);
  await check(page.locator('#document-status')).toHaveText('Unsaved changes');
  await page.locator('#open-agents').click();
  await check(page.locator('#agents')).toBeVisible();
  await page.locator('#open-workbench').click();
  await check(editor).toHaveValue(edited);
  const gateway = '**/forge/v1/dispatch';
  const operations = [];
  const loseSaveReply = async route => {
    const body = route.request().postDataJSON();
    if (body?.kind !== 'edit_write') return route.continue();
    operations.push([body.request.intent.kind, body.request.operation]);
    if (body.request.intent.kind !== 'save_document') return route.continue();
    const response = await route.fetch();
    assert.equal((await response.json()).event.outcome.kind, 'applied');
    return route.abort('failed');
  };
  await context.route(gateway, loseSaveReply);
  try {
    await page.getByRole('button', { name: 'Save file', exact: true }).click();
    await check(page.locator('#workbench-write-status')).toContainText('Write outcome unknown');
    await page.getByRole('button', { name: 'Check status', exact: true }).click();
    await check(source).toContainText('saved with direct editor');
    assert.deepEqual(operations.map(([kind]) => kind), ['save_document', 'status']);
    assert.equal(operations[0][1], operations[1][1]);
    await screenshot(page, 'ide-workbench-managed-applied.png');
  } finally { await context.unroute(gateway, loseSaveReply); }
  await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
  await check(page.locator('#workbench')).toBeVisible();
  await check(source).toContainText('saved with direct editor');
  await check(page.locator('#workbench-write-status')).toContainText(/Applied · rev [0-9a-f]{8}/);
  await page.getByRole('button', { name: 'Undo edit', exact: true }).click();
  await check(source.locator('[data-line="2"]')).toContainText('old line');
  await check(page.locator('#workbench-write-status')).toContainText(/Reverted · rev [0-9a-f]{8}/);
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
    'Character creation', 'Tools', 'Design documentation',
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
  await check(launchers).toHaveCount(4);
  await check(docs).toHaveCount(8);
  await check(launchers.locator('svg[aria-hidden="true"]')).toHaveCount(4);
  await check(page.locator('.home-launcher-agents')).toHaveAttribute('href','/agents');
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
    ['Explore Scout character creation', '/design/characters'],
    ['Forge IDE', '/'],
    ['Forge Studio', '/studio'],
    ['Room One playtest', '/playtest'],
    ['Forge plug-and-play', '/design/forge-providers'],
    ['Forge foundations', '/design#forge-foundations'],
    ['Conversation storage', '/design#chat-and-mail'],
    ['Game development', '/design#game-development'],
    ['Wireframes & IDE screens', '/design#wireframes-and-workspace'],
    ['Coding-agent dispatch design', '/design/coding-agents'],
    ['Agent usage visibility design', '/design/agent-usage'],
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
      const designHome = page.getByRole('navigation', { name: 'Forge views' }).getByRole('link', { name: 'Home', exact: true });
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
  await assertRetiredChat(page, traffic);
  assert.equal(await tabKey(), originalTab, 'returning from Home restores the same tab workspace');
  assert.equal(page.workers().length, 1, 'returning from Home starts one Forge worker');
  await open('/home', () => brandLink.click());
  await open('/', () => primary.click());
  await check(page.locator('#workspace-panel')).toBeVisible();
  await assertRetiredChat(page, traffic);
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
  const views = page.getByRole('navigation', { name: 'Forge views' });
  const topics = page.getByRole('navigation', { name: 'Design topics' });
  await check(views.getByRole('link')).toHaveText(portalViewNames);
  await check(views.getByRole('link', { name: 'Design', exact: true })).toHaveAttribute('aria-current', 'page');
  for (const [name, href] of [['Agents', '/#agents'], ['Workbench', '/#workbench']]) {
    await check(views.getByRole('link', { name, exact: true })).toHaveAttribute('href', href);
  }
  await check(topics.getByRole('link')).toHaveCount(4);
  const designIndex = page.getByRole('navigation', { name: 'Design sections' });
  await check(designIndex.getByRole('heading', { level: 2 })).toHaveText([
    'Forge foundations', 'Chat and Mail', 'Game development', 'Wireframes and workspace',
  ]);
  const providerLink = designIndex.locator('a[href="/design/forge-providers"]');
  await check(providerLink).toContainText('Forge plug-and-play');
  await check(providerLink).not.toHaveAttribute('target', /.+/);
  await check(page.locator('#previous-chat-menu, figure.wireframe, script')).toHaveCount(0);

  process.stderr.write('QA_CHECK design_phone_layout\n');
  for (const viewport of [{ width: 390, height: 664 }, { width: 375, height: 667 }]) {
    await page.setViewportSize(viewport);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'Design index fits the phone viewport');
    assert.equal(await designIndex.getByRole('region').evaluateAll(panels => {
      const boxes = panels.map(panel => panel.getBoundingClientRect());
      return boxes.slice(1).every((box, index) => box.top >= boxes[index].bottom - 1);
    }), true, 'design index categories stack on phones');
  }
  await page.evaluate(() => scrollTo(0, 0));
  await screenshot(page, 'ide-design-index-phone.png', { docs: true });
  await designIndex.locator('a[href="/design/workspace"]').scrollIntoViewIfNeeded();
  assert.equal(await topics.evaluate(nav => {
    const box = nav.getBoundingClientRect();
    return getComputedStyle(nav).position === 'sticky' && box.top >= -1 && box.bottom <= innerHeight;
  }), true, 'topic navigation remains available while the index scrolls');

  process.stderr.write('QA_CHECK design_desktop_layout\n');
  await page.setViewportSize({ width: 1440, height: 1000 });
  assert.equal(await designIndex.getByRole('region').evaluateAll(([first, second]) =>
    first.offsetTop === second.offsetTop && first.offsetLeft < second.offsetLeft),
  true, 'design index categories display side by side on desktop');
  await page.evaluate(() => scrollTo(0, 0));
  await screenshot(page, 'ide-design-index-desktop.png', { docs: true });
  await characterDesignJourney(page, designUrl);
  await implementationDesignJourney(page, designUrl);
  await agentDesignTablesJourney(page, designUrl);
  await wireframesDesignJourney(page, designUrl);
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
  await page.getByRole('navigation', { name: 'Forge views' }).getByRole('link', { name: 'Workbench', exact: true }).click();
  await check(page).toHaveURL(new URL('/', originalUrl).href);
  await check(page.locator('#workbench')).toBeVisible();
  await assertRetiredChat(page, traffic);
  assert.equal(await tabKey(), originalTab, 'the IDE returns to the same tab workspace');
  assert.deepEqual(context.pages(), pages, 'returning from Design uses the existing page');
  await check(page.locator('#workspace-panel')).toBeVisible();
  assert.equal(page.workers().length, 1, 'returning from Design starts one Forge worker');
}

async function characterDesignJourney(page, designUrl) {
  process.stderr.write('QA_CHECK design_character_creation\n');
  const pages = page.context().pages();
  const index = page.getByRole('navigation', { name: 'Design sections' });
  const link = index.locator('#game-development a[href="/design/characters"]');
  await check(link).toContainText('Character creation');
  await check(link).not.toHaveAttribute('target', /.+/);
  await link.focus();
  await page.keyboard.press('Enter');
  await check(page).toHaveURL(new URL('/design/characters', designUrl).href);
  assert.deepEqual(page.context().pages(), pages, 'the character showcase opens in the existing Design page');
  const main = page.getByRole('main');
  await check(main.getByRole('heading', { level: 1 })).toHaveText('Scout character creation');
  await check(main.locator('.character-progression figure')).toHaveCount(3);
  await check(main.locator('.character-clip')).toHaveCount(16);
  await check(page.locator('script')).toHaveCount(0);
  await check(page.getByRole('navigation', { name: 'Design topics' })
    .getByRole('link', { name: 'Game Dev', exact: true })).toHaveAttribute('aria-current', 'location');
  const clips = main.locator('details.character-clip');
  await check(clips).toHaveCount(16);
  assert.equal(await clips.evaluateAll(nodes => nodes.every(node => !node.open)), true,
    'motion previews wait for an explicit native disclosure action');
  for (const image of await main.locator('img:visible').all()) {
    await check(image).toHaveAttribute('src', /^\/design\/characters\//);
    await check(image).toHaveAttribute('alt', /\S/);
    await image.scrollIntoViewIfNeeded();
    await check.poll(() => image.evaluate(node => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0)).toBe(true);
  }
  for (const clip of await clips.all()) {
    const summary = clip.locator(':scope > summary');
    await summary.focus();
    await page.keyboard.press('Enter');
    await check(clip).toHaveJSProperty('open', true);
    const motion = clip.locator('img.character-motion');
    await check(motion).toBeVisible();
    for (const image of await clip.locator('img:visible').all()) {
      await check(image).toHaveAttribute('src', /^\/design\/characters\//);
      await check(image).toHaveAttribute('alt', /\S/);
      await image.scrollIntoViewIfNeeded();
      await check.poll(() => image.evaluate(node => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0)).toBe(true);
    }
    await summary.focus();
    await page.keyboard.press('Enter');
    await check(clip).toHaveJSProperty('open', false);
  }
  const firstClip = clips.first();
  await firstClip.locator(':scope > summary').click();
  await check(firstClip.locator('img.character-motion')).toBeVisible();
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await check(firstClip.locator('img.character-motion')).toBeHidden();
  const still = firstClip.locator('img.character-motion-still');
  await check(still).toBeVisible();
  await check.poll(() => still.evaluate(image => image.complete && image.naturalWidth > 0)).toBe(true);
  assert.equal(await main.locator('*').evaluateAll(nodes => nodes.every(node => {
    const style = getComputedStyle(node);
    return (style.animationName === 'none' || style.animationPlayState === 'paused' || parseFloat(style.animationDuration) <= .001)
      && style.transitionDuration.split(',').every(value => parseFloat(value) === 0);
  })), true, 'character controls respect reduced motion alongside the still preview');
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await firstClip.locator(':scope > summary').click();
  for (const width of [375, 390, 1440]) {
    await page.setViewportSize({ width, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'character art and frame strips keep overflow inside the showcase');
  }
  for (const [width, name] of [[390, 'phone'], [1440, 'desktop']]) {
    await page.setViewportSize({ width, height: 844 });
    await page.evaluate(() => scrollTo(0, 0));
    await screenshot(page, `ide-design-characters-${name}.png`, { docs: true });
  }
  await page.getByRole('navigation', { name: 'Breadcrumb' }).getByRole('link', { name: 'Back to Design', exact: true }).click();
  await check(page).toHaveURL(`${designUrl}#game-development`);
  assert.deepEqual(page.context().pages(), pages, 'leaving the showcase retains the existing page');
}

async function implementationDesignJourney(page, designUrl) {
  process.stderr.write('QA_CHECK design_implementation_reader\n');
  const pages = page.context().pages();
  const index = page.getByRole('navigation', { name: 'Design sections' });
  const firstFoundation = index.locator('#forge-foundations li a').first();
  await check(firstFoundation).toHaveAttribute('href', '/design/implementation');
  await firstFoundation.focus();
  await page.keyboard.press('Enter');
  await check(page).toHaveURL(new URL('/design/implementation', designUrl).href);
  assert.deepEqual(page.context().pages(), pages, 'the current implementation reader opens in the existing Design page');
  const main = page.getByRole('main');
  await check(main.getByRole('heading', { level: 1 })).toHaveText('Current Forge implementation');
  await check(main.getByRole('table')).toHaveCount(3);
  const report = main.locator('article.implementation-report');
  await check(report).toHaveCount(1);
  await check(report).toContainText('Active navigation is Agents, Workbench, Home and Design');
  await check(report).toContainText('with Agents as the default admitted view');
  const browserContract = report.getByRole('row').filter({ has: page.getByRole('cell', { name: 'Browser application', exact: true }) });
  await check(browserContract).toContainText('browser ABI is 15');
  await check(report).toContainText('The retained pre-delivery live checkpoint is main 95651a5 (tree 6d380c5a)');
  await check(report).toContainText('actual built revision a3a14e9');
  await check(report).toContainText('499 consumed build inputs');
  await check(report).toContainText('The Agents project/task delivery panel and protected Root receipts are the runtime authority for the latest source, checks and activation');
  await check(report).toContainText('updating this reference does not establish a new rollout');
  const lastDeployment = report.locator('p').filter({ hasText: 'That Web-only rollout preserved' });
  await check(lastDeployment).toContainText('PostgreSQL schema26');
  await check(lastDeployment).toContainText('no Studio relay');
  await check(lastDeployment).toContainText('neither managed-write opt-in nor write grants');
  const mailBudget = report.getByRole('row').filter({ has: page.getByRole('cell', { name: 'Historical Mail token profile', exact: true }) });
  await check(mailBudget).toContainText('4,096 context / 1,024 completion');
  await check(mailBudget).toContainText('dated profile observations');
  const editing = report.getByRole('row').filter({ has: page.getByRole('cell', { name: 'Editing', exact: true }) });
  await check(editing).toContainText('revision/journal checks');
  await check(editing).toContainText('not enabled for the last checked live');
  await check(page.locator('#previous-chat-menu, script')).toHaveCount(0);
  for (const width of [375, 390, 1440]) {
    await page.setViewportSize({ width, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'the implementation reference keeps tables inside its reading viewport');
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await page.evaluate(() => scrollTo(0, 0));
  const navigationSequence = page.locator('img[src="/design/portal-navigation-sequence.svg"]');
  await navigationSequence.scrollIntoViewIfNeeded();
  await check.poll(() => navigationSequence.evaluate(image => image.complete && image.naturalWidth > 0)).toBe(true);
  await page.evaluate(() => scrollTo(0, 0));
  await screenshot(page, 'ide-design-implementation-phone.png', { docs: true });
  const historyLink = main.getByRole('link', { name: 'Chat history', exact: true });
  await check(historyLink).toHaveAttribute('href', '/design/chat-history');
  await check(main.locator('a[target]')).toHaveCount(0);
  await historyLink.focus();
  await page.keyboard.press('Enter');
  await check(page).toHaveURL(new URL('/design/chat-history', designUrl).href);
  await check(main.getByRole('heading', { level: 1 })).toHaveText('Previous chats technical design');
  assert.deepEqual(page.context().pages(), pages, 'trusted Markdown reader links use the existing Design page');
  await page.goBack({ waitUntil: 'domcontentloaded' });
  await check(page).toHaveURL(new URL('/design/implementation', designUrl).href);
  await page.getByRole('navigation', { name: 'Breadcrumb' }).getByRole('link', { name: 'Back to Design', exact: true }).click();
  await check(page).toHaveURL(`${designUrl}#forge-foundations`);
}

async function agentDesignTablesJourney(page, designUrl) {
  process.stderr.write('QA_CHECK design_agent_table_scroll\n');
  for (const path of ['/design/coding-agents', '/design/agent-usage']) {
    await page.getByRole('navigation', { name: 'Design sections' }).locator(`a[href="${path}"]`).click();
    await check(page).toHaveURL(new URL(path, designUrl).href);
    const tables = page.locator('main .engine-table');
    for (const width of [320, 390, 1440]) {
      await page.setViewportSize({ width, height: 844 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
        'wide comparison tables scroll locally without widening the page');
      for (const table of await tables.all()) {
        await check(table).toHaveAttribute('role', 'region');
        await check(table).toHaveAttribute('aria-label', /\S/);
        await check(table).toHaveAttribute('tabindex', '0');
        assert.equal(await table.locator('th, td').evaluateAll(cells => cells.every(cell => {
          const style = getComputedStyle(cell);
          return cell.getBoundingClientRect().width >= 160
            && style.wordBreak === 'normal' && style.overflowWrap === 'normal';
        })), true, 'prose and identifier columns retain readable widths and normal word wrapping');
        if (width < 640) {
          const cueId = await table.getAttribute('aria-describedby');
          await check(page.locator(`#${cueId}`)).toBeVisible();
          assert.equal(await table.evaluate(node => node.scrollWidth > node.clientWidth), true);
          await table.evaluate(node => { node.scrollLeft = 0; });
          await table.focus();
          await check(table).toBeFocused();
          await page.keyboard.press('ArrowRight');
          await check.poll(() => table.evaluate(node => node.scrollLeft)).toBeGreaterThan(0);
          await table.evaluate(node => { node.scrollLeft = node.scrollWidth; });
          assert.equal(await table.evaluate(node => {
            const edge = node.getBoundingClientRect().right;
            return [...node.querySelectorAll('tr')].every(row => row.lastElementChild.getBoundingClientRect().right <= edge + 1);
          }), true, 'horizontal scrolling exposes the last column instead of clipping it permanently');
          await table.evaluate(node => { node.scrollLeft = 0; });
        }
      }
    }
    await page.getByRole('link', { name: 'Back to Design', exact: true }).click();
    await check(page).toHaveURL(`${designUrl}#forge-foundations`);
  }
}

async function wireframesDesignJourney(page, designUrl) {
  process.stderr.write('QA_CHECK design_wireframes_reader\n');
  const context = page.context();
  const pages = context.pages();
  await page.getByRole('navigation', { name: 'Design sections' }).locator('a[href="/design/wireframes#phone"]').click();
  await check(page).toHaveURL(new URL('/design/wireframes#phone', designUrl).href);
  await check(page.getByRole('heading', { level: 1 })).toHaveText('IDE wireframes');
  await check(page.locator('#previous-chat-menu')).toHaveCount(0);
  await check(page.locator('.wireframe-note')).toContainText('18 November 2026');
  await check(page.locator('.wireframe-note')).toContainText('generated UI');
  // Dated reference captures include retired Mail screens; image decoding here
  // is documentation coverage, not acceptance of a live Mail application.
  const names = ['chat-phone', 'chat-history-phone', 'workbench-phone', 'workbench-explorer-phone', 'workbench-project-options', 'mail-inbox-phone', 'mail-thread-phone', 'chat-desktop', 'workbench-desktop', 'mail-thread-desktop'];
  const images = page.locator('figure.wireframe img');
  await check(images).toHaveCount(names.length);
  for (const name of names) {
    const path = `/design/forge-ide-${name}.png`;
    const image = page.locator(`figure.wireframe img[src="${path}"]`);
    await check(image).toHaveAttribute('alt', /\S/);
    await check(page.locator(`figure.wireframe a[href="${path}"]`).filter({ has: page.locator('img') }))
      .not.toHaveAttribute('target', /.+/);
    await image.scrollIntoViewIfNeeded();
    await check.poll(() => image.evaluate(node => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0)).toBe(true);
    assert.equal(await image.evaluate(node => {
      const stage = node.closest('.wireframe-stage');
      if (!stage) return false;
      const style = getComputedStyle(stage);
      return style.backgroundColor !== 'rgba(0, 0, 0, 0)' && parseFloat(style.paddingTop) >= 8;
    }), true, 'the current implementation capture has a padded themed frame');
  }
  for (const viewport of [{ width: 390, height: 664 }, { width: 375, height: 667 }]) {
    await page.setViewportSize(viewport);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
      'wireframe reader fits the phone viewport');
    assert.equal(await images.evaluateAll(nodes => nodes.every(node => {
      const image = node.getBoundingClientRect();
      return image.width > 0 && image.height > 0 && image.left >= -1 && image.right <= innerWidth + 1;
    })), true, 'each framed wireframe fits horizontally');
    assert.equal(await page.locator('#phone figure.wireframe').evaluateAll(figures => {
      const boxes = figures.map(figure => figure.getBoundingClientRect());
      return boxes.slice(1).every((box, index) => box.top >= boxes[index].bottom - 1);
    }), true, 'phone wireframe previews stack vertically');
  }
  await page.evaluate(() => scrollTo(0, 0));
  await screenshot(page, 'ide-design-wireframes-phone.png', { docs: true });
  await page.setViewportSize({ width: 1440, height: 1000 });
  assert.equal(await page.locator('#desktop figure.wireframe').evaluateAll(([first, second]) => {
    const left = first.getBoundingClientRect();
    const right = second.getBoundingClientRect();
    return Math.abs(left.top - right.top) <= 1 && left.right <= right.left + 1 && right.right <= innerWidth + 1;
  }), true, 'desktop wireframe previews display side by side');
  await page.getByRole('link', { name: 'Back to Design', exact: true }).click();
  await check(page).toHaveURL(/\/design(?:#wireframes-and-workspace)?$/);
  await page.getByRole('navigation', { name: 'Design sections' }).locator('a[href="/design/workspace"]').click();
  await check(page).toHaveURL(new URL('/design/workspace', designUrl).href);
  await check(page.getByRole('heading', { level: 1 })).toHaveText('Workspace data flow');
  await check(page.getByRole('heading', { name: 'Workspace recovery', exact: true })).toBeVisible();
  await page.getByRole('link', { name: 'Back to Design', exact: true }).click();
  // Let destination loading and fragment navigation settle before the caller
  // transfers keyboard focus; otherwise navigation can overwrite that focus.
  await page.waitForURL(`${designUrl}#wireframes-and-workspace`, { waitUntil: 'load', timeout: 10_000 });
  await check(page.getByRole('heading', { level: 1, name: 'Forge IDE design', exact: true })).toBeVisible();
  await check(page.locator('#wireframes-and-workspace')).toBeInViewport();
  assert.deepEqual(context.pages(), pages, 'all documentation readers use the existing page');
}

async function designLocalRestoreJourney(page) {
  process.stderr.write('QA_CHECK design_local_workspace_restore\n');
  const originalUrl = page.url();
  const title = await page.locator('#selection-title').textContent();
  const context = page.context();
  await page.getByRole('link', { name: 'Design', exact: true }).click();
  await check(page).toHaveURL(new URL('/design', originalUrl).href);
  const handoff = await page.evaluate(() => JSON.parse(sessionStorage.getItem('forge-workspace-navigation')));
  assert.equal(handoff?.version, 1);
  assert.equal(handoff.tab, await page.evaluate(() => sessionStorage.getItem('forge-workspace-tab')));
  assert.equal(handoff.snapshot.workbench_open, true);
  assert.ok(handoff.snapshot.workbench?.entry, 'the local checkpoint retains the selected-file bookmark');
  const workspaceRoute = '**/forge/v1/dispatch/workspace';
  let rejectedRestore = false;
  const rejectRestore = async route => {
    if (route.request().postDataJSON()?.operation === 'restore') {
      rejectedRestore = true;
      await route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
    } else await route.continue();
  };
  await context.route(workspaceRoute, rejectRestore);
  try {
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await check(page).toHaveURL(originalUrl);
    await check(page.locator('#workbench')).toBeVisible();
    await check(page.locator('#selection-title')).toHaveText(title);
    await check(page.locator('#source-view')).not.toHaveText('');
    await assertRetiredChat(page);
    assert.equal(rejectedRestore, true, 'browser Back restores the local bookmark and re-verifies its source after remote restore failure');
  } finally { await context.unroute(workspaceRoute, rejectRestore); }
}

async function previousChatsDesignJourney(design, designUrl) {
  process.stderr.write('QA_CHECK design_previous_chats\n');
  await design.getByRole('navigation', { name: 'Design sections' }).locator('a[href="/design/previous-chats"]').click();
  await check(design).toHaveURL(new URL('/design/previous-chats', designUrl).href);
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

  await check(design.getByRole('heading', { level: 1, name: 'Previous chats', exact: true })).toBeVisible();
  await check(proposal.locator('.sandbox-toolbar')).toContainText(/interactive preview/i);
  await check(proposal.getByRole('table').locator('thead').getByRole('columnheader')).toHaveText(['State', 'Behavior']);
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

  const reset = design.getByRole('link', { name: 'Reset preview', exact: true });
  await reset.focus();
  await check(reset).toBeFocused();
  const [resetResponse] = await Promise.all([
    design.waitForResponse(response => response.request().isNavigationRequest() && response.url() === new URL('/design/previous-chats', designUrl).href),
    design.keyboard.press('Enter'),
  ]);
  assert.equal(resetResponse.status(), 200, 'Reset reloads the owned preview document');
  await check(row('workbench')).toHaveCount(1);
  await check(title).toHaveText('Summarize Workbench');
  await check(draft).toHaveValue('Add a note about Explorer.');
  await check(menu).toHaveJSProperty('open', true);
  await design.evaluate(() => scrollTo(0, 0));
  await screenshot(design, 'ide-design-preview-phone.png', { docs: true });

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
  await check(design.locator('.intro')).toContainText('Implemented dispatch mode.');
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
  const landingLink = design.getByRole('navigation', { name: 'Design sections' }).locator('a[href="/design/chat-dispatch"]');
  await check(landingLink).toHaveAttribute('href', '/design/chat-dispatch');
}

async function gameEngineDesignJourney(page, designUrl) {
  const pages = page.context().pages();
  const link = page.getByRole('navigation', { name: 'Design sections' }).locator('a[href="/design/game-studio"]');
  await check(page.getByRole('navigation', { name: 'Design sections' }).locator('a[href="/playtest"]'))
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

  await check(report.getByRole('link', { name: 'Character creation', exact: true })).toHaveAttribute('href', '/design/characters');
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

async function retiredChatJourney(page, traffic) {
  await assertRetiredChat(page, traffic);
  for (const event of [
    {kind:'open_chat'}, {kind:'show_chat_history'}, {kind:'send_chat'},
    {kind:'new_chat'}, {kind:'cancel_chat_run'}, {kind:'retry_chat_submit'},
  ]) await retiredEvent(page,event);
  await check(page.locator('#agents')).toBeVisible();
  await assertApplicationViews(page,'Agents');
  await assertRetiredChat(page,traffic);
  await page.locator('#open-workbench').click();
  await check(page.locator('#workbench')).toBeVisible();
}

// Controlled workspace fixture: real UI/WASM and admitted Work relay remain in
// use; only workspace restore/save is overridden with owned boundary-size data.
async function oversizedDormantWorkspaceJourney(page, context, fixture, relay, traffic) {
  process.stderr.write('QA_CHECK oversized_dormant_workspace\n');
  const conversation = {
    messages: [{role:'user',content:'\\'.repeat(2048)},{role:'assistant',content:'\\'.repeat(4096)}],
    draft:'\\'.repeat(2048),
  };
  const chat = {
    selected:0,model_id:'qwen2.5-coder-7b',
    conversations:Array.from({length:16},()=>structuredClone(conversation)),
    pending_run:{chat_id:'11111111-1111-4111-8111-111111111111',
      idempotency_key:'22222222-2222-4222-8222-222222222222',
      run_id:'33333333-3333-4333-8333-333333333333',expected_revision:4,model_id:'qwen2.5-coder-7b'},
  };
  chat.conversations[0].chat_id=chat.pending_run.chat_id;
  chat.conversations[0].revision=4;
  assert(Buffer.byteLength(JSON.stringify(chat))>256*1024,
    'valid escaped Chat fields exceed the navigation-cache budget');
  let durable={chat:structuredClone(chat),workbench:null,workbench_open:false};
  let restores=0,unavailableRestores=0,savesDuringOutage=0,restoreUnavailable=false,submitted=null;
  const workspaceUrl=fixture.origin+'/forge/v1/dispatch/workspace';
  const gatewayUrl=fixture.origin+'/forge/v1/dispatch';
  const workspace=async route=>{
    const body=route.request().postDataJSON();
    assert.equal(body.preserve_chat,true);
    if(body.operation==='restore'){
      assert.equal('history' in body || 'models' in body,false);
      if(restoreUnavailable){
        unavailableRestores++;
        await route.fulfill({status:503,contentType:'application/json',body:'{}'});
      }else{
        restores++;
        await route.fulfill({json:{lease:'a'.repeat(64),snapshot:structuredClone(durable)}});
      }
    }else{
      assert.equal(body.operation,'save');
      if(restoreUnavailable)savesDuringOutage++;
      assert.deepEqual(body.snapshot.chat,chat,'saving active work cannot truncate dormant Chat');
      durable=structuredClone(body.snapshot);
      await route.fulfill({json:{}});
    }
  };
  const observeSubmission=async route=>{
    const body=route.request().postDataJSON();
    if(body.kind==='work_submit'){
      const cache=await page.evaluate(()=>JSON.parse(sessionStorage.getItem('forge-workspace-navigation')));
      assert.equal(cache.snapshot,null,'oversized duplicate data is omitted from the bounded local cache');
      assert.deepEqual(cache.agents.retry_intent,body,'the complete task intent is cached before Work I/O');
      submitted=structuredClone(body);
    }
    await route.fallback();
  };
  // Leave the prior small snapshot before installing the large restored data.
  await page.locator('#forge-home').click();
  await check(page).toHaveURL(fixture.origin+'/home');
  await context.route(workspaceUrl,workspace);
  await context.route(gatewayUrl,observeSubmission);
  try{
    await page.locator('.home-launcher-agents').click();
    await check(page.locator('#agents')).toBeVisible();
    await check(page.locator('#agents-new')).toBeEnabled();
    await assertRetiredChat(page,traffic);
    await page.locator('#agents-new').click();
    const prompt='Boundary-size dormant Chat must not block this new assignment.';
    await page.locator('#agents-prompt').fill(prompt);
    await page.locator('#agents-criteria').fill('Preserve full dormant data and exact task retry identity.');
    await check(page.locator('#agents-send')).toBeEnabled();
    await page.locator('#agents-send').click();
    await check.poll(()=>submitted!==null).toBe(true);
    await check(page.locator('#agents-reader')).toBeVisible();
    await check(page.locator('#agents-task-prompt')).toHaveText(prompt);
    assert.equal(relay.workflowSubmissions.at(-1).idempotency_key,submitted.idempotency_key);
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin+'/home');
    const cache=await page.evaluate(()=>JSON.parse(sessionStorage.getItem('forge-workspace-navigation')));
    assert.equal(cache.snapshot,null);
    assert.equal(cache.agents.task_id,relay.latestWorkflowTaskId);
    await page.getByRole('link',{name:'Browse design docs',exact:true}).click();
    await check(page).toHaveURL(fixture.origin+'/design');
    await assertNativeViews(page,'Design');
    // The server now refuses restoration. A cache with no workspace body must
    // not synthesize a lease or write an empty replacement over durable Chat.
    restoreUnavailable=true;
    await page.getByRole('navigation',{name:'Forge views'}).getByRole('link',{name:'Agents',exact:true}).click();
    await check(page.locator('#agents-reader')).toBeVisible();
    await check(page.locator('#agents-task-prompt')).toHaveText(prompt);
    assert.equal(unavailableRestores,1);
    assert.equal(savesDuringOutage,0,'unavailable restore cannot admit a workspace write');
    assert.ok(restores>=1);
    assert.deepEqual(durable.chat,chat,'the full retained Chat snapshot remains unchanged');
    await assertRetiredChat(page,traffic);
  }finally{
    await context.unroute(workspaceUrl,workspace);
    await context.unroute(gatewayUrl,observeSubmission);
  }
}

async function museDatabaseDirectoryJourney(page) {
  process.stderr.write('QA_CHECK muse_database_directory\n');
  const viewport=page.viewportSize();
  const card=page.locator('#agents-overview #agents-external-agents > #agents-muse-db');
  await check(card).toBeVisible();
  await check(card.getByRole('heading',{name:'Muse',exact:true})).toBeVisible();
  await check(card).toContainText('PostgreSQL · external runner');
  await check(card.locator('#agents-muse-db-state')).toHaveText('No runner heartbeat');
  await check(card).not.toContainText(/@gmail\.com/i);
  await check(card).toContainText('Muse owns its runner. Forge dispatches assignments and receives plans and review reports through PostgreSQL.');
  await check(card.locator('button,a,input,textarea,select,[contenteditable],[data-action]')).toHaveCount(0);
  await check(card.locator('meter,progress,.fg-quota,.fg-metric,[data-card-id]')).toHaveCount(0);
  await check(card).not.toContainText(/\b(?:Idle|Ready|Quota|Tokens?)\b/i);
  await check(page.locator('#agents-providers #agents-muse-db')).toHaveCount(0);
  await check(page.locator('#agents-profile option').filter({hasText:/muse/i})).toHaveCount(0);
  for(const width of [320,390,1440]){
    await page.setViewportSize({width,height:844});
    await card.scrollIntoViewIfNeeded();
    await check(card).toBeInViewport();
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,
      'the external contact directory fits phone and desktop viewports');
    assert.equal(await card.evaluate(node=>{
      const bounds=node.getBoundingClientRect();
      if(bounds.left< -1 || bounds.right>innerWidth+1)return false;
      const walker=document.createTreeWalker(node,NodeFilter.SHOW_TEXT);
      for(let text=walker.nextNode();text;text=walker.nextNode()){
        if(!text.textContent.trim())continue;
        const range=document.createRange();range.selectNodeContents(text);
        if([...range.getClientRects()].some(rect=>rect.left< -1 || rect.right>innerWidth+1))return false;
      }
      return true;
    }),true,'external runner details wrap without clipped text');
  }
  if(viewport)await page.setViewportSize(viewport);
  await page.locator('#agents-heading').scrollIntoViewIfNeeded();
}

async function grokProviderJourney(page) {
  process.stderr.write('QA_CHECK grok_configured_provider\n');
  const viewport=page.viewportSize();
  const card=page.locator('#agents-providers article[data-card-id="grok-primary"]');
  await check(card.locator('.fg-metric-select')).toHaveText('Grok');
  await check(card.locator('.fg-metric-header > .fg-badge')).toHaveText('Unknown');
  await check(card.locator('.fg-metric-observation > .fg-badge')).toHaveText('Unknown');
  await check(card.locator('.fg-quota-unavailable')).toContainText('Not reported');
  await check(card.locator('.fg-quota-unavailable')).toContainText('No verified allowance observation.');
  await check(card.locator('meter,progress')).toHaveCount(0);
  await check(card.locator('.fg-metric dd')).toHaveText(['Unknown','Unknown','Unknown','Unknown']);
  for(const width of [320,390,1440]){
    await page.setViewportSize({width,height:844});
    await card.scrollIntoViewIfNeeded();
    await check(card).toBeInViewport();
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,
      'configured Grok usage fits phone and desktop viewports');
    assert.equal(await card.locator('.fg-metric dd').evaluateAll(nodes=>nodes.every(node=>{
      const range=document.createRange();range.selectNodeContents(node);
      return range.getClientRects().length===1;
    })),true,'unknown token observations stay on one readable line');
    assert.equal(await card.evaluate(node=>{
      const bounds=node.getBoundingClientRect();
      if(bounds.left< -1 || bounds.right>innerWidth+1)return false;
      const walker=document.createTreeWalker(node,NodeFilter.SHOW_TEXT);
      for(let text=walker.nextNode();text;text=walker.nextNode()){
        if(!text.textContent.trim())continue;
        const range=document.createRange();range.selectNodeContents(text);
        if([...range.getClientRects()].some(rect=>rect.left< -1 || rect.right>innerWidth+1))return false;
      }
      return true;
    }),true,'Grok card labels and unknown metrics remain readable');
  }
  await card.locator('.fg-metric-select').focus();
  await page.keyboard.press('Enter');
  const details=card.locator('details.fg-metric-details');
  const disclosure=details.locator('summary');
  await check(disclosure).toBeVisible();
  await disclosure.focus();
  await page.keyboard.press('Enter');
  await check(details).toHaveJSProperty('open',true);
  await check(details).toContainText('Grok / owned-browser-fixture');
  await check(details).toContainText('grok-4.7');
  await page.keyboard.press('Enter');
  await check(details).toHaveJSProperty('open',false);
  await page.locator('#agents-providers article[data-card-id="agy-primary"] .fg-metric-select').click();
  if(viewport)await page.setViewportSize(viewport);
  await page.locator('#agents-heading').scrollIntoViewIfNeeded();
}

async function agentsJourney() {
  process.stderr.write('QA_CHECK portal_agents_workflow\n');
  agentsRelayOwner = acquireOwned(() => startAgentsRelay(), relay => relay.close());
  let fixture, context;
  let releaseUsageOverview = () => {};
  let releaseAssignmentCatalog = () => {};
  try {
    const relay = await agentsRelayOwner.ready;
    fixture = await startFixture(false, false, { agents: relay });
    context = await browser.newContext({ viewport: { width: 390, height: 844 } });
    const traffic = await observe(context);
    const { page, errors, respondToLeaveWarning } = await connect(context, fixture);
    async function chooseTaskScope(value) {
      await check(page.locator('#agents-task-scope')).toBeEnabled();
      await page.locator('#agents-task-scope').selectOption(value);
      await check(page.locator('#agents-task-scope')).toBeEnabled();
      await check(page.locator('#agents-task-scope')).toHaveValue(value);
    }
    await check.poll(traffic.catalogMailEnabled).toBe(false);
    await assertApplicationViews(page, 'Workbench');
    await page.locator('#open-agents').focus();
    await page.keyboard.press('Enter');
    await check(page.locator('#agents')).toBeVisible();
    await check(page.locator('#agents-task-scope')).toHaveValue('active');
    await check(page.locator('#agents-tasks')).toContainText('No active assignments');
    await chooseTaskScope('attention');
    await check(page.locator('#agents-task-scope option[value="attention"]')).toHaveText('Needs attention (1)');
    await check(page.locator('#agents-tasks')).toContainText('Fixture existing report');
    await assertApplicationViews(page, 'Agents');
    await page.locator('#open-workbench').click();
    await check(page.locator('#workbench')).toBeVisible();
    await page.locator('#open-agents').click();
    await check(page.locator('#agents')).toBeVisible();
    await assertApplicationViews(page, 'Agents');
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin + '/home');
    await assertNativeViews(page, 'Home');
    await page.locator('.home-launcher-agents').focus();
    await page.keyboard.press('Enter');
    await check(page.locator('#agents')).toBeVisible();
    await check(page.locator('#agents-worker')).toHaveText('Worker ready');
    await assertRetiredChat(page, traffic);
    await assertApplicationViews(page, 'Agents');
    await check(page.locator('#ide-navigation > :is(button,a)')).toHaveCount(4);
    await chooseTaskScope('attention');
    await check(page.locator('#agents-tasks')).toContainText('Fixture existing report');
    const cards = page.locator('#agents-providers article.fg-metric-card');
    const geminiCard = page.locator('#agents-providers article[data-card-id="agy-primary"]');
    const codexCard = page.locator('#agents-providers article[data-card-id="codex-primary"]');
    await check(cards).toHaveCount(3);
    await museDatabaseDirectoryJourney(page);
    await grokProviderJourney(page);
    await check(geminiCard.locator('.fg-metric-select')).toHaveText('Gemini');
    await check(codexCard.locator('.fg-metric-select')).toHaveText('Codex');
    await check(geminiCard.locator('.fg-quota-unavailable')).toContainText('does not expose account allowance');
    await check(geminiCard.locator('meter')).toHaveCount(0);
    await check(geminiCard.locator('.fg-metric-header > .fg-badge')).toHaveText('Idle');
    await check(codexCard.locator('.fg-metric-observation > .fg-badge')).toHaveText('Fresh');
    const quota = codexCard.locator('meter.fg-quota-meter');
    await check(quota).toHaveJSProperty('min', 0);
    await check(quota).toHaveJSProperty('max', 100);
    await check(quota).toHaveJSProperty('value', 12.5);
    await check(quota).toHaveAttribute('aria-label', 'Provider window quota used');
    await check(quota).toHaveAttribute('aria-valuetext', '12.5% used');
    await check(codexCard.locator('.fg-quota > time')).toHaveAttribute('title', '2026-11-19T19:30:00Z');
    const total = codexCard.locator('.fg-metric dd').first();
    assert.equal((await total.getAttribute('title')).replace(/[^0-9]/g, ''), '9007199254740998',
      'wide token counts retain every exact digit alongside their compact KPI');
    assert.notEqual((await total.textContent()).replace(/[^0-9]/g, ''), '9007199254740998');
    const seed = page.locator('#agents-tasks [data-row-id="11111111-1111-4111-8111-111111111111"]');
    await check(seed.locator('.fg-row-summary .fg-badge')).toHaveText('Ready for review');
    await check(seed.locator('.fg-row-summary .fg-badge')).toHaveAttribute('data-tone', 'accent');
    await check(seed.locator('time.fg-row-time')).toHaveAttribute('title', '2026-11-19T18:00:00Z');
    await check(seed.locator('time.fg-row-time')).toHaveAttribute('datetime', '2026-11-19T18:00:00Z');
    assert.equal(await page.locator('#agents time').evaluateAll(nodes => nodes.every(node => !/\d{4}-\d{2}-\d{2}T/.test(node.textContent))), true,
      'raw ISO dates do not appear in primary rows or card summaries');
    await check(page.locator('#agents-worker')).toHaveAttribute('data-tone', 'success');
    const initialLayout = await Promise.all(['#agents-heading', '#agents-worker', '#agents-tasks .fg-tree-row'].map(selector => page.locator(selector).first().boundingBox()));
    assert(Math.abs(initialLayout[0].y + initialLayout[0].height / 2 - initialLayout[1].y - initialLayout[1].height / 2) < 20,
      'worker status is inline with the Agents heading');
    assert(initialLayout[2].y < 380 && initialLayout[2].y + initialLayout[2].height < 844,
      'the first task is readable above the phone fold');
    await check(page.locator('#agents-new')).toBeEnabled();
    const newTaskBox = await page.locator('#agents-new').boundingBox();
    assert(Math.abs(newTaskBox.y + newTaskBox.height / 2 - initialLayout[0].y - initialLayout[0].height / 2) < 20, 'New task shares the compact Agents header row');

    // A finished task has no polling lifecycle; opening it must still read its
    // persisted public plan/history rather than waiting for a state revision.
    await seed.locator('.fg-row-select').click();
    await check(page.locator('#agents-reader')).toBeVisible();
    await check(page.locator('#agents-muse-db')).toBeHidden();
    await check(page.locator('#agents-task-status')).toContainText('Ready for review');
    await check(page.locator('#agents-plan')).toContainText('Public fixture plan');
    await check(page.locator('#agents-progress')).toContainText('Public fixture milestone');
    await check(page.locator('#agents-progress-panel')).not.toContainText('FOREIGN ATTEMPT PROGRESS MUST NOT APPEAR');
    await check(page.locator('#agents-progress-panel script,#agents-progress-panel img')).toHaveCount(0);
    await page.locator('#agents-back').click();
    await check(page.locator('#agents-overview')).toBeVisible();

    await page.locator('#agents-new').click();
    await check(page.locator('#agents-muse-db')).toBeHidden();
    // Keep the existing result-acceptance journey on a Review task. Finished
    // Plan tasks now use the explicit artifact-bound decision flow below.
    await page.locator('#agents-mode').selectOption('review');
    const prompt = page.locator('#agents-prompt');
    const criteria = page.locator('#agents-criteria');
    const retained = 'Keep this local Agents draft. A long task title preserves useful context across the dashboard without pushing status or timestamps out of view.';
    await prompt.fill(retained);
    await criteria.fill('Recover the draft after portal navigation.');
    await check(page.locator('#agents-send')).toBeEnabled();
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin + '/home');
    await assertNativeViews(page, 'Home');
    const retiredMail = legacyMailNavigation();
    await page.evaluate(({ mail, draft }) => {
      const key = 'forge-workspace-navigation';
      const cache = JSON.parse(sessionStorage.getItem(key));
      if (cache?.agents?.prompt !== draft || cache.agents.active !== true) throw new Error('Expected genuine Agents handoff');
      cache.mail = mail;
      sessionStorage.setItem(key, JSON.stringify(cache));
    }, { mail: retiredMail, draft: retained });
    // The legacy entry resolves to Agents without activating/polling old Mail.
    await page.goto(fixture.origin + '/#mail', { waitUntil: 'domcontentloaded' });
    await check(page.locator('#agents-compose')).toBeVisible();
    await check(page).toHaveURL(fixture.origin + '/');
    await assertApplicationViews(page, 'Agents');
    await check(prompt).toHaveValue(retained);
    await check(criteria).toHaveValue('Recover the draft after portal navigation.');
    await page.locator('#forge-home').click();
    await check(page).toHaveURL(fixture.origin + '/home');
    const inertMail = await page.evaluate(() => JSON.parse(sessionStorage.getItem('forge-workspace-navigation'))?.mail);
    assert.deepEqual(inertMail, { ...retiredMail, active: false },
      'Rust retains valid legacy subject/draft as inert recovery data');
    await page.locator('.home-launcher-agents').click();
    await check(page.locator('#agents-compose')).toBeVisible();
    await check(prompt).toHaveValue(retained);
    await respondToLeaveWarning(() => page.goBack({ waitUntil: 'domcontentloaded' }), 'leave');
    await check(page).toHaveURL(fixture.origin + '/home');
    await page.goForward({ waitUntil: 'domcontentloaded' });
    await check(page.locator('#agents-compose')).toBeVisible();
    await assertApplicationViews(page, 'Agents');
    await check(prompt).toHaveValue(retained);
    await check(criteria).toHaveValue('Recover the draft after portal navigation.');
    assert.equal(traffic.businessCalls().mail, 0, 'legacy cached Mail never emits a Mail gateway request');

    // The main thread acknowledges opaque Rust recovery data before Work IO.
    let checkpoints = 0;
    const refreshedProfiles = [];
    let holdUsageOverview = false, usageOverviewHeld = false;
    const usageOverviewGate = new Promise(resolve => { releaseUsageOverview = resolve; });
    let holdAssignmentCatalog=false, assignmentCatalogHeld=false;
    const assignmentCatalogGate=new Promise(resolve=>{releaseAssignmentCatalog=resolve;});
    await context.route(fixture.origin + '/forge/v1/dispatch', async route => {
      const request = route.request().postDataJSON();
      if (request.kind === 'usage_refresh') refreshedProfiles.push({ profile: request.agent_profile, project: request.project_alias });
      if(request.kind==='work_catalog' && holdAssignmentCatalog){
        holdAssignmentCatalog=false;assignmentCatalogHeld=true;
        await assignmentCatalogGate;
      }
      if (request.kind === 'usage_overview' && holdUsageOverview) {
        holdUsageOverview = false;
        usageOverviewHeld = true;
        await usageOverviewGate;
      }
      if (['work_submit', 'work_cancel', 'work_accept', 'work_resume', 'work_plan_decide'].includes(request.kind)) {
        const saved = await page.evaluate(intent => {
          const cache = JSON.parse(sessionStorage.getItem('forge-workspace-navigation'));
          return cache?.agents?.retry_intent?.kind === intent.kind
            && cache.agents.retry_intent.idempotency_key === intent.idempotency_key;
        }, request);
        assert.equal(saved, true, 'mutating request identity is cached before network dispatch');
        checkpoints++;
      }
      await route.continue();
    });
    relay.faults.denyNextSubmit = true;
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-status')).toContainText('not authorized');
    await check(page.locator('#agents-compose')).toBeVisible();
    await check(page.locator('#connection')).toBeHidden();
    await check(page.locator('#agents-send')).toBeEnabled();
    assert.equal(relay.taskCount, 1, 'typed403 does not expire the session or admit a task');

    relay.faults.dropNextSubmit = true;
    relay.faults.holdProgressCompletion = true;
    const progressRefetches = relay.progressRefetches;
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-retry')).toBeVisible();
    await check(page.locator('#open-agents')).toBeDisabled();
    await check(prompt).toBeDisabled();
    assert.equal(relay.taskCount, 2);
    await page.locator('#forge-home').click();
    await check(page.locator('[data-workspace-navigation-status]')).toContainText('still open');
    await check(page.locator('#agents')).toBeVisible();
    await page.locator('#agents-retry').click();
    await check(page.locator('[data-workspace-navigation-status]')).toBeHidden();
    await check(page.locator('#agents-reader')).toBeVisible();
    await check(page.locator('#agents-plan')).toContainText('Public fixture plan');
    await check(page.locator('#agents-plan h2')).toHaveText('Public fixture plan');
    await check(page.locator('#agents-progress')).toContainText('Public fixture milestone');
    await check(page.locator('#agents-task-status')).toContainText('Running');
    await check(page.locator('#agents-accept')).toBeHidden();
    await check(page.locator('#agents-result-panel')).toBeHidden();
    await check(page.locator('#agents-attempt')).toContainText('Progress saved');
    await check.poll(() => relay.progressRefetches).toBeGreaterThan(progressRefetches);
    await check(page.locator('#agents-progress-panel')).not.toContainText('FOREIGN ATTEMPT PROGRESS MUST NOT APPEAR');
    await check(page.locator('#agents-progress-panel script,#agents-progress-panel img')).toHaveCount(0);
    assert.equal(await page.evaluate(() => globalThis.__forgeProgressExecuted), undefined,
      'public Markdown progress is inert text and grants no execution or terminal status');
    await disclosure(page, 'agents-progress-details', true);
    await check(page.locator('#agents-progress')).toBeVisible();
    await screenshot(page, 'ide-agents-progress-phone.png');
    relay.faults.holdProgressCompletion = false;
    await check(page.locator('#agents-task-status')).toContainText('Ready for review');
    await check(page.locator('#agents-result')).toContainText('Verified first section.');
    assert.equal(relay.taskCount, 2, 'unknown-outcome retry reuses the committed admission');
    await check(page.locator('#agents-result script,#agents-result img')).toHaveCount(0);
    let pages = 1;
    while (pages < 16 && await page.locator('#agents-result-next').isEnabled()) {
      const before = await page.locator('#agents-result-page').textContent();
      await page.locator('#agents-result-next').click();
      await check.poll(() => page.locator('#agents-result-page').textContent()).not.toBe(before);
      pages++;
    }
    assert(pages >= 5, 'a response larger than64KiB is read through bounded pages');
    await check(page.locator('#agents-result')).toContainText('Final fixture marker.');
    await check(page.locator('#agents-result-next')).toBeDisabled();
    await page.locator('#agents-result-previous').click();
    await check(page.locator('#agents-result-next')).toBeEnabled();
    await page.locator('#agents-review-note').fill('Reviewed the fixture result.');
    await page.locator('#agents-accept').click();
    await check(page.locator('#agents-task-status')).toContainText('Accepted');
    await check(page.locator('#agents-accept')).toBeHidden();
    const historyReads = relay.calls.filter(kind => kind === 'work_events').length;
    const acceptedTitle = await page.locator('#agents-task-title').innerText();
    await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
    // Reload restores the earlier local handoff draft. Reopen the saved task
    // from the list; progress history comes from PostgreSQL, not that draft.
    await check(page.locator('#agents')).toBeVisible();
    await page.locator('#agents-back').click();
    await check(page.locator('#agents-overview')).toBeVisible();
    await chooseTaskScope('history');
    await page.locator('#agents-tasks .fg-row-select').filter({ hasText: acceptedTitle }).click();
    await check(page.locator('#agents-reader')).toBeVisible();
    await check(page.locator('#agents-task-status')).toContainText('Accepted');
    await check(page.locator('#agents-plan')).toContainText('Public fixture plan');
    await check(page.locator('#agents-progress')).toContainText('Public fixture milestone');
    await check.poll(() => relay.calls.filter(kind => kind === 'work_events').length).toBeGreaterThan(historyReads);
    await check(page.locator('#agents-progress-panel')).not.toContainText('FOREIGN ATTEMPT PROGRESS MUST NOT APPEAR');
    await check(page.locator('#agents-progress-panel script,#agents-progress-panel img')).toHaveCount(0);
    assert.equal(await page.evaluate(() => globalThis.__forgeProgressExecuted), undefined);

    relay.faults.holdProgressCompletion = true;
    await page.locator('#agents-new').click();
    await prompt.fill('Cancel fixture work');
    await criteria.fill('Confirm that the worker stops.');
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-task-status')).toContainText('Running');
    await page.locator('#agents-cancel').click();
    await check(page.locator('#agents-task-status')).toContainText('Stopping');
    await check(page.locator('#agents-task-status')).toContainText('Cancelled');
    relay.faults.holdProgressCompletion = false;
    await page.locator('#agents-back').click();
    await check(page.locator('#agents-providers')).toBeVisible();
    const submissions = relay.calls.filter(kind => kind === 'work_submit').length;
    await codexCard.locator('.fg-metric-select').click();
    await check(codexCard).toHaveAttribute('data-selected', 'true');
    const details = codexCard.locator('details.fg-metric-details');
    const summary = details.locator(':scope > summary');
    const closedChevron = await summary.evaluate(node => getComputedStyle(node, '::after').transform);
    assert.notEqual(closedChevron, 'none', 'Gemini disclosure cue is styled before opening');
    await summary.focus();
    await page.keyboard.press('Enter');
    await check(details).toHaveJSProperty('open', true);
    await check.poll(() => summary.evaluate(node => getComputedStyle(node, '::after').transform)).not.toBe(closedChevron);
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await check.poll(() => summary.evaluate(node => getComputedStyle(node, '::after').transitionDuration)).toBe('0s');
    await page.emulateMedia({ reducedMotion: 'no-preference' });
    await check(codexCard.locator('.fg-metric-observation > .fg-metric-time')).toHaveCSS('text-align', 'end');
    await check(details).toContainText('9007199254740998');
    await check(details.locator('code')).not.toHaveCount(0);
    await check(geminiCard.locator('details.fg-metric-details')).toBeHidden();
    await check(page.locator('#agents-usage-more,#agents-usage-details,#agents-usage-refresh')).toHaveCount(0);
    await check(page.locator('#agents-providers button button')).toHaveCount(0);
    const detailsNode = await details.elementHandle();
    const refresh = codexCard.locator('.fg-metric-refresh');
    await check(refresh).toBeEnabled();
    await check(refresh).toHaveAttribute('aria-label', 'Refresh Codex usage');
    holdUsageOverview = true;
    await refresh.click();
    await check.poll(() => relay.refreshes).toBe(1);
    await check.poll(() => usageOverviewHeld).toBe(true);
    await summary.focus();
    await check(summary).toBeFocused();
    releaseUsageOverview();
    await check(page.locator('#agents-usage-status')).toBeHidden();
    await check(refresh).toBeEnabled();
    await check(details).toHaveJSProperty('open', true);
    await check(summary).toBeFocused();
    assert.equal(await details.evaluate((node, original) => node === original, detailsNode), true,
      'accepted metadata updates preserve the same native disclosure');
    await detailsNode.dispose();
    assert.deepEqual(refreshedProfiles, [{ profile: 'codex-primary', project: 'workspace' }],
      'the adjacent refresh action carries its own admitted profile');
    assert.equal(relay.calls.filter(kind => kind === 'work_submit').length, submissions,
      'metadata refresh never starts a generation');
    await screenshot(page, 'ide-agents-accounting-phone.png');
    await summary.focus();
    await page.keyboard.press('Enter');
    await check(details).toHaveJSProperty('open', false);
    await chooseTaskScope('history');
    const accepted = page.locator('#agents-tasks .fg-tree-row').filter({ hasText: 'Keep this local Agents draft.' });
    await check(accepted.locator('.fg-row-summary .fg-badge')).toHaveText('Accepted');
    await check(accepted.locator('.fg-row-summary .fg-badge')).toHaveAttribute('data-tone', 'success');
    const cancelled = page.locator('#agents-tasks .fg-tree-row').filter({ hasText: 'Cancel fixture work' });
    await check(cancelled.locator('.fg-row-summary .fg-badge')).toHaveText('Cancelled');

    for (const width of [320, 390, 1440]) {
      await page.setViewportSize({ width, height: 844 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
        'Agents controls, task rows and usage cards fit the viewport');
      assert.equal(await cards.evaluateAll((nodes, phone) => {
        const [a, b] = nodes.map(node => node.getBoundingClientRect());
        return phone ? b.top >= a.bottom - 1 : Math.abs(a.top - b.top) <= 1;
      }, width < 800), true, 'provider cards stack on phones and share a desktop row');
      assert.equal(await accepted.locator('.fg-row-label').evaluate(node => {
        const style = getComputedStyle(node);
        return style.webkitLineClamp === '2' && node.getBoundingClientRect().height <= 2 * parseFloat(style.lineHeight) + 1;
      }), true, 'long task titles occupy at most two readable lines');
      for (const control of [codexCard.locator('.fg-metric-refresh'), summary]) {
        const box = await control.boundingBox();
        assert(box && box.width >= 44 && box.height >= 44, 'card actions retain touch-sized targets');
      }
      await page.locator('#agents-new').focus();
      await check(page.locator('#agents-new')).toBeFocused();
    }
    for (const [width, name] of [[390, 'phone'], [1440, 'desktop']]) {
      await page.setViewportSize({ width, height: 844 });
      await page.locator('#agents').evaluate(node => { node.scrollTop = 0; });
      await screenshot(page, `ide-agents-overview-${name}.png`);
    }
    process.stderr.write('QA_CHECK plan_decision_lifecycle\n');
    await page.setViewportSize({ width: 390, height: 844 });
    async function openFinishedPlan(status, title) {
      const taskId = relay.addPlan(status, title);
      await chooseTaskScope(status==='completed'?'history':'attention');
      await page.locator('#agents-refresh').click();
      const row = page.locator(`#agents-tasks [data-row-id="${taskId}"] .fg-row-select`);
      await check(row).toBeVisible(); await row.click();
      await check(page.locator('#agents-plan-actions')).toBeVisible();
      await check(page.locator('#agents-stage-panel')).toBeVisible();
      await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText(status === 'completed' ? 'Accepted' : 'Ready for review');
      await check(page.locator('#agents-stage-diagram script,#agents-stage-diagram img')).toHaveCount(0);
      await check(page.locator('#agents-plan-resume')).toBeEnabled();
      await check(page.locator('#agents-plan-resume')).toHaveText('Resume');
      await check(page.locator('#agents-plan-revise')).toHaveText('Revise');
      await check(page.locator('#agents-plan-cancel')).toHaveText('Cancel');
      await check(page.locator('#agents-accept')).toBeHidden();
      await check(page.locator('#agents-resume')).toBeHidden();
      await check(page.locator('#agents-plan-revise')).toBeDisabled();
      return taskId;
    }
    const approvedPlan = await openFinishedPlan('awaiting_review', 'Plan approval and PR fixture');
    relay.faults.dropNextPlanDecision = true;
    relay.faults.holdPublication = true;
    await page.locator('#agents-plan-auto-repair').check();
    await page.locator('#agents-plan-resume').click();
    await check(page.locator('#agents-retry')).toBeVisible();
    await check(page.locator('#open-agents')).toBeDisabled();
    const originalDecision = relay.planDecisions.at(-1);
    assert.equal(originalDecision.decision, 'resume');
    assert.equal(originalDecision.auto_repair,true);
    assert.equal(originalDecision.task_id, approvedPlan);
    assert.match(originalDecision.plan_sha256, /^[0-9a-f]{64}$/);
    await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
    await check(page.locator('#agents-retry')).toBeVisible();
    await check(page.locator('#agents-retry')).toBeEnabled();
    await page.locator('#agents-retry').click();
    await check(page.locator('#agents-retry')).toBeHidden();
    await check(page.locator('#agents-plan-actions')).toBeHidden();
    await check(page.locator('#agents-verification-status')).toHaveText('Awaiting independent review');
    await check(page.locator('#agents-verification-pipeline')).toBeVisible();
    await check(page.locator('#agents-verification-pipeline-heading')).toHaveText('Pipeline');
    assert.deepEqual(
      await page.locator('#agents-verification-pipeline-diagram .fg-flow-node .fg-flow-label').allTextContents(),
      ['source and Python preflight','make ci','in-memory backend'],
      'pipeline diagram renders branch gate stages in plan order',
    );
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[aria-current="step"]')).toContainText('make ci');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="source-preflight"]')).toHaveAttribute('data-state','completed');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="in-memory-backend"]')).toHaveAttribute('data-state','upcoming');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-badge')).toHaveText('Running · make ci');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-summary')).toHaveText('Verification is running. 1 of 3 stages passed.');
    await check(page.locator('#agents-verification-pipeline-diagram script,#agents-verification-pipeline-diagram img')).toHaveCount(0);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'pipeline diagram fits the viewport');
    const verificationTarget=relay.requireVerificationRetry(approvedPlan);
    const tasksBeforeVerification=relay.taskCount;
    const submissionsBeforeVerification=relay.calls.filter(kind=>kind==='work_submit').length;
    const verificationEffectsBefore=relay.verificationRetries.length;
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-verification-status')).toHaveText('Coordinator handoff');
    await check(page.locator('#agents-review-retry')).toBeEnabled();
    await page.locator('#agents-review-note').fill('Retry verification only against the same finished author source.');
    await page.locator('#agents-review-retry').click();
    await check(page.locator('#agents-verification-status')).toHaveText('Awaiting independent review');
    relay.setPipeline(approvedPlan,[
      {id:'source-preflight',label:'source and Python preflight',status:'passed'},
      {id:'check',label:'make ci',status:'passed'},
      {id:'in-memory-backend',label:'in-memory backend',status:'running'},
    ],'available','running');
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[aria-current="step"]')).toContainText('in-memory backend');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="check"]')).toHaveAttribute('data-state','completed');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-badge')).toHaveText('Running · in-memory backend');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-summary')).toHaveText('Verification is running. 2 of 3 stages passed.');
    relay.setPipeline(approvedPlan,[
      {id:'source-preflight',label:'source and Python preflight',status:'passed'},
      {id:'check',label:'make ci',status:'failed',finding_id:'compiler-fixture',finding_summary:'Required compiler fixture finding'},
      {id:'in-memory-backend',label:'in-memory backend',status:'not_run'},
    ],'available','complete');
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="check"]')).toHaveAttribute('data-state','error');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="check"] .fg-flow-detail')).toHaveText('compiler-fixture · Required compiler fixture finding');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="in-memory-backend"]')).toHaveAttribute('data-state','skipped');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-node[data-stage-id="in-memory-backend"] .fg-flow-detail')).toHaveText('not run');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-badge')).toHaveText('Complete · 1 stage failed');
    await check(page.locator('#agents-verification-pipeline-diagram .fg-flow-summary')).toHaveText('Verification complete. 1 of 3 stages passed.');
    await check(page.locator('#agents-retry')).toBeHidden();
    await check(page.locator('#agents-review-retry')).toBeHidden();
    await check(page.locator('#agents-review-note')).toHaveValue('');
    await check.poll(()=>relay.verificationRetries.length).toBe(verificationEffectsBefore+1);
    const verificationRetry=relay.verificationRetries.at(-1);
    assert.equal(verificationRetry.request.decision,'retry_infrastructure');
    assert.equal(verificationRetry.request.project_alias,'workspace');
    assert.equal(verificationRetry.request.task_id,approvedPlan);
    assert.equal(verificationRetry.request.expected_revision,verificationTarget.revision);
    assert.equal(verificationRetry.request.review_id,verificationTarget.review.review_id);
    assert.equal(typeof verificationRetry.request.request_id,'string');
    assert(verificationRetry.request.request_id.length>0);
    assert.equal(verificationRetry.receipt.base_revision,verificationTarget.revision);
    assert.equal(verificationRetry.receipt.revision,verificationTarget.revision+1);
    assert.notEqual(verificationRetry.receipt.review_id,verificationTarget.review.review_id);
    assert.match(verificationRetry.receipt.review_id,/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/);
    await check.poll(()=>relay.reviewReads.some(value=>value.task_id===approvedPlan
      &&value.review_id===verificationRetry.receipt.review_id&&value.revision===verificationRetry.receipt.revision)).toBe(true);
    const verifiedTask=relay.reviewSnapshot(approvedPlan);
    assert.equal(verifiedTask.attempt.attempt_id,verificationTarget.attempt.attempt_id);
    assert.equal(verifiedTask.review.source_attempt_id,verificationTarget.review.source_attempt_id);
    assert.equal(verifiedTask.review.round,verificationTarget.review.round);
    assert.equal(verifiedTask.agent_profile,verificationTarget.agent_profile);
    assert.equal(relay.taskCount,tasksBeforeVerification,'verification retry never creates an author task');
    assert.equal(relay.calls.filter(kind=>kind==='work_submit').length,submissionsBeforeVerification,
      'verification retry does not submit another assignment');
    assert.equal(relay.reviewDecisions.filter(value=>value.task_id===approvedPlan&&value.decision==='retry_infrastructure').length,1);
    await check(page.locator('#agents-review-approve')).toBeDisabled();
    await page.locator('#agents-verification-details summary').click();
    await check(page.locator('#agents-verification-findings')).toContainText('compiler-fixture');
    await page.locator('#agent-finding-0').check();
    await check(page.locator('#agents-review-approve')).toBeDisabled();
    await page.locator('#agents-review-note').fill('I independently verified the required compiler check on this exact candidate.');
    await check(page.locator('#agents-review-approve')).toBeEnabled();
    await page.locator('#agents-review-approve').click();
    await check(page.locator('#agents-publication-status')).toContainText('Publication pending');
    assert.deepEqual(relay.reviewDecisions.at(-1).verified_finding_ids,['compiler-fixture']);

    await check(page.locator('#agents-task-mode')).toHaveText('Mode · Implement');
    await check(page.locator('#agents-accept')).toBeHidden();
      await check(page.locator('#agents-resume')).toBeHidden();
    const retriedDecisions = relay.planDecisions.filter(value => value.task_id === approvedPlan);
    assert.equal(retriedDecisions.length, 2);
    for (const value of retriedDecisions) {
      assert.equal(value.idempotency_key, originalDecision.idempotency_key);
      assert.equal(value.expected_revision, originalDecision.expected_revision);
      assert.equal(value.plan_attempt_id, originalDecision.plan_attempt_id);
      assert.equal(value.plan_sha256, originalDecision.plan_sha256);
      assert.equal(value.auto_repair,true);
    }
    relay.faults.holdPublication = false;
    await check(page.locator('#agents-publication-status')).toHaveText('Pull request published', { timeout: 15000 });
    await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText('Publish');
    await check(page.locator('#agents-accept')).toBeVisible();
    await check(page.locator('#agents-accept')).toBeEnabled();
    const pullRequest = page.locator('#agents-publication-link a');
    await check(pullRequest).toHaveAttribute('href', 'https://github.com/roachmaster/homelab/pull/42');
    await check(pullRequest).toHaveAttribute('rel', 'noopener noreferrer');
    await check(pullRequest).toHaveAttribute('target', '_blank');
    assert(relay.publicationRevisions.length >= 4);
    assert.equal(new Set(relay.publicationRevisions).size, 1, 'publication polling observes same-revision changes');
    for (const width of [320, 390, 1440]) {
      await page.setViewportSize({ width, height: 844 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true);
      const box = await pullRequest.boundingBox(); assert(box && box.height >= 44);
    }
    await page.setViewportSize({ width: 390, height: 844 });
    await page.locator('#agents-back').click();
    await openFinishedPlan('completed', 'Legacy completed plan revision fixture');
    const feedback = 'Add concrete failure-path tests before implementation.';
    await page.locator('#agents-review-note').fill(feedback);
    await check(page.locator('#agents-plan-revise')).toBeEnabled();
    await page.locator('#agents-plan-revise').click();
    await check(page.locator('#agents-plan-actions')).toBeHidden();
    await check(page.locator('#agents-task-status')).toHaveText('Ready for review');
    await check(page.locator('#agents-plan-actions')).toBeVisible();
    assert.equal(relay.planDecisions.at(-1).decision, 'revise');
    assert.equal(relay.planDecisions.at(-1).note, feedback);
    await page.locator('#agents-plan-cancel').click();
    await check(page.locator('#agents-task-status')).toHaveText('Cancelled');
    await check(page.locator('#agents-plan-actions')).toBeHidden();
    assert.equal(relay.planDecisions.at(-1).decision, 'cancel');
    await page.locator('#agents-back').click();
    process.stderr.write('QA_CHECK autonomous_agent_workflow\n');
    relay.enableAutonomy();holdAssignmentCatalog=true;
    await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
    await check(page.locator('#agents')).toBeVisible();
    await check.poll(()=>assignmentCatalogHeld).toBe(true);
    await check(page.locator('#agents-new')).toBeDisabled();
    await check(page.locator('#agents-compose')).toBeHidden();
    releaseAssignmentCatalog();
    await page.locator('#agents-new').click();
    await check(page.locator('#agents-manual-assignment')).toBeHidden();
    await check(page.locator('#agents-compose-repair-policy')).toBeHidden();
    await check(page.locator('#agents-assignment-roles')).toHaveText('Gemini authors · Codex reviews');
    await check(page.locator('#agents-compose-workflow-docs')).toHaveAttribute('href','/design/autonomous-agents');
    await page.locator('#agents-prompt').fill('Owned autonomous workflow fixture; no real agent is invoked.');
    await page.locator('#agents-criteria').fill('Keep peer review and automatic merge source-bound.');
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Reviewing plan');
    await check(page.locator('#agents-stage-diagram .fg-flow-track-label')).toHaveText('Autonomous workflow · Native');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText('Plan review');
    const autonomousSubmission=relay.workflowSubmissions.at(-1);
    assert.equal(autonomousSubmission.workflow_policy_digest,'c'.repeat(64));
    assert.equal(autonomousSubmission.mode,'plan');assert.equal(autonomousSubmission.agent_profile,'agy-primary');
    const workflowTaskId=relay.latestWorkflowTaskId;
    for(const id of ['agents-plan-actions','agents-review','agents-accept','agents-resume'])await check(page.locator('#'+id)).toBeHidden();
    await page.locator('#agents-back').click();
    const workflowRow=page.locator(`#agents-tasks [data-row-id="${workflowTaskId}"]`);
    await check(workflowRow.locator('.fg-row-summary .fg-badge')).toHaveText('Reviewing plan');
    relay.workflowPhase(workflowTaskId,'checks');
    await check(workflowRow.locator('.fg-row-summary .fg-badge')).toHaveText('Running checks');
    await workflowRow.locator('.fg-row-select').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Running checks');
    for(const phase of ['code_review','hosted','merging','completed']){
      relay.workflowPhase(workflowTaskId,phase);await page.locator('#agents-refresh').click();
      const phaseLabel={code_review:'Reviewing code',hosted:'Hosted checks pending',merging:'Merging',completed:'Merged'}[phase];
      const stageLabel={code_review:'Code review',hosted:'Hosted',merging:'Merge',completed:'Merged'}[phase];
      await check(page.locator('#agents-workflow-phase')).toHaveText(phaseLabel);
      await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText(stageLabel);
      if(phase==='code_review'){
        for(const [width,name] of [[390,'phone'],[1440,'desktop']]){
          await page.setViewportSize({width,height:844});
          assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'stage diagram fits the viewport');
          await page.locator('#agents-stage-panel').scrollIntoViewIfNeeded();
          await screenshot(page,`ide-agents-stage-diagram-${name}.png`);
        }
        await page.setViewportSize({width:390,height:844});
      }
      await check(page.locator('#agents-accept')).toBeHidden();
      await check(page.locator('#agents-plan-actions')).toBeHidden();
      if(['merging','completed'].includes(phase))await check(page.locator('#agents-cancel')).toBeHidden();
      await page.locator('#agents-back').click();
      if(phase==='completed')await chooseTaskScope('history');
      await check(workflowRow).toContainText(phaseLabel);
      await check(workflowRow).not.toContainText('Ready for review');
      await workflowRow.locator('.fg-row-select').click();
      await check(page.locator('#agents-workflow-phase')).toHaveText(phaseLabel);
    }
    await check(page.locator('#agents-delivery')).toBeHidden();
    for(const [stage,label] of [['queued','Main verification queued'],['running','Verifying main'],['repair','Fixing tests · Retry 1/20'],['deployed','Deployed']]){
      relay.setDelivery(workflowTaskId,stage);
      await page.locator('#agents-refresh').click();
      await check(page.locator('#agents-workflow-phase')).toHaveText('Merged');
      await check(page.locator('#agents-delivery-state')).toHaveText(label);
      await check(page.locator('#agents-task-status')).toHaveText(label);
      const disclosure=page.locator('#agents-delivery-details');
      if(await disclosure.getAttribute('open')===null)await disclosure.locator('summary').click();
      await check(page.locator('#agents-delivery-verification-summary')).toContainText('Build 42');
      await check(page.locator('#agents-delivery-summary')).not.toContainText('2026-11-19T');
      await check(page.locator('#agents-delivery-source')).toContainText('Main commit · '+'f'.repeat(40));
      for(const width of [320,390,1440]){
        await page.setViewportSize({width,height:844});
        assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'delivery stage and source fit the viewport');
      }
      await page.setViewportSize({width:390,height:844});
      if(stage==='repair'){
        await check(page.locator('#agents-delivery-repair-summary')).toContainText('Fixing tests · Retry 1/20');
        await check(page.locator('#agents-delivery-repair-task')).toBeEnabled();
        await check(page.locator('#agents-stage-diagram .fg-flow-branch[data-branch-id="delivery_repair"]')).toContainText('Round 1/20');
      }
      if(stage==='deployed'){
        await check(page.locator('#agents-delivery-state')).toHaveAttribute('data-tone','success');
        await check(page.locator('#agents-delivery-source')).toContainText('Activation receipt · '+'e'.repeat(64));
        await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="delivery_deploy"]')).toHaveAttribute('data-state','completed');
        await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="delivery_deploy"]')).toHaveAttribute('aria-current','step');
      }
    }
    // Exact receipt assertions above stay read-only; keep hash-shaped values
    // behind their native disclosure when producing shareable screenshots.
    const deliveryDetails=page.locator('#agents-delivery-details');
    if(await deliveryDetails.getAttribute('open')!==null)await deliveryDetails.locator('summary').click();
    await page.locator('#agents-delivery-state').scrollIntoViewIfNeeded();
    await screenshot(page,'ide-agents-delivery-deployed.png');
    const deliveryTaskCount=relay.taskCount;
    relay.setProjectDelivery('deployed');
    await page.locator('#agents-back').click();
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-project-delivery-state')).toHaveText('Deployed');
    assert.equal(relay.taskCount,deliveryTaskCount,'observed main delivery does not fabricate a coding task');
    await workflowRow.locator('.fg-row-select').click();
    for(const width of [320,390,1440]){
      await page.setViewportSize({width,height:844});
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'workflow roles and counters fit the viewport');
    }
    await page.locator('#agents-back').click();await page.locator('#agents-new').click();
    await page.locator('#agents-prompt').fill('Owned escalation and cancellation fixture.');
    await page.locator('#agents-criteria').fill('Preserve the source and expose cancellation after escalation.');
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Reviewing plan');
    await check(page.locator('#agents-stage-diagram .fg-flow-track-label')).toHaveText('Autonomous workflow · Native');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText('Plan review');
    const escalationTaskId=relay.latestWorkflowTaskId;
    relay.workflowPhase(escalationTaskId,'escalated');await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Needs attention');
    await check(page.locator('#agents-stage-diagram .fg-flow-branch[data-kind="exception"]')).toContainText('Needs attention');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="merged"]')).toHaveAttribute('data-state','skipped');
    await check(page.locator('#agents-workflow-escalation')).toContainText('Revision budget exhausted');
    await check(page.locator('#agents-accept')).toBeHidden();await check(page.locator('#agents-plan-actions')).toBeHidden();
    await page.locator('#agents-back').click();
    await chooseTaskScope('attention');
    const escalationRow=page.locator(`#agents-tasks [data-row-id="${escalationTaskId}"]`);
    await check(escalationRow).toContainText('Needs attention');
    await escalationRow.locator('.fg-row-select').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Needs attention');
    await page.locator('#agents-cancel').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Cancelled');
    await check(page.locator('#agents-stage-diagram .fg-flow-branch[data-kind="exception"]')).toContainText('Cancelled');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="merged"]')).toHaveAttribute('data-state','skipped');
    await oversizedDormantWorkspaceJourney(page, context, fixture, relay, traffic);
    relay.enableExternal();
    await respondToLeaveWarning(() => page.reload({ waitUntil: 'domcontentloaded' }), 'leave');
    if(await page.locator('#agents-back').isVisible())await page.locator('#agents-back').click();
    await page.locator('#agents-new').click();
    await check(page.locator('#agents-manual-assignment')).toBeVisible();
    await page.locator('#agents-profile').selectOption('muse-db-primary');
    await check(page.locator('#agents-compose-help')).toContainText('Muse owns its runner');
    await page.locator('#agents-prompt').fill('Create a bounded external plan using the database handoff.');
    await page.locator('#agents-criteria').fill('Return the plan without source or merge authority.');
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-external-result-panel')).toBeVisible();
    await check(page.locator('#agents-external-result h1')).toHaveText('External Muse plan');
    await check(page.locator('#agents-external-result script,#agents-external-result img')).toHaveCount(0);
    assert.equal(await page.evaluate(()=>globalThis.__externalExecuted),undefined);
    for(const id of ['agents-progress-panel','agents-result-panel','agents-accept','agents-resume','agents-plan-actions'])await check(page.locator('#'+id)).toBeHidden();
    for(const width of [320,390,1440]){
      await page.setViewportSize({width,height:844});
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'external artifact fits the viewport');
    }
    await screenshot(page,'ide-agents-external-muse.png');
    await page.locator('#agents-back').click();
    await check(page.locator('#agents-muse-db-state')).toHaveText('Runner available');
    // The full envelope history is separate from the bounded current-attempt milestones.
    const historyTaskId=relay.addHistory();
    await chooseTaskScope('attention');
    await page.locator('#agents-refresh').click();
    await page.locator(`#agents-tasks [data-row-id="${historyTaskId}"] .fg-row-select`).click();
    await check(page.locator('#agents-task-correlation')).toContainText('Correlation ·');
    await check(page.locator('#agents-history > li')).toHaveCount(50);
    await check(page.locator('#agents-history-range')).toHaveText('Events 1–50');
    await check(page.locator('#agents-stage-diagram .fg-flow-summary')).toContainText('Events 1–61');
    await check(page.locator('#agents-stage-diagram script,#agents-stage-diagram img')).toHaveCount(0);
    const stageTrace=page.locator('#agents-stage-diagram details.fg-flow-trace');
    await stageTrace.locator('summary').focus();await page.keyboard.press('Enter');
    await check(stageTrace).toHaveJSProperty('open',true);
    await check(stageTrace.locator('.fg-flow-hop-exact').first()).toBeVisible();
    await check(stageTrace.locator('.fg-flow-hop-exact').first()).toHaveText('2026-11-19T18:00:00Z');
    const stageTraceNode=await stageTrace.elementHandle();
    const eventDetails=page.locator('#agents-history > li').first().locator('details');
    await eventDetails.locator('summary').focus();await page.keyboard.press('Enter');
    await check(eventDetails).toHaveJSProperty('open',true);
    const eventNode=await eventDetails.elementHandle();
    const historyResponse = afterSequence => page.waitForResponse(response => {
      if (new URL(response.url()).pathname !== '/forge/v1/dispatch') return false;
      const request = response.request().postDataJSON();
      return request?.kind === 'work_events' && request.task_id === historyTaskId
        && request.after_sequence === afterSequence;
    }, { timeout: 10000 });
    await check(page.locator('#agents-refresh')).toBeEnabled();
    // Refresh also reads unseen milestones at cursor61, independently of the visible page.
    const refreshedHistory = historyResponse(61);
    await page.locator('#agents-refresh').click();
    const refreshResponse = await refreshedHistory;
    assert.equal(refreshResponse.status(),200);
    assert.equal(await refreshResponse.finished(),null);
    await check(page.locator('#agents-refresh')).toBeEnabled();
    await check(page.locator('#agents-history-next')).toBeEnabled();
    await check(eventDetails).toHaveJSProperty('open',true);
    assert.equal(await eventNode.evaluate(node=>node.isConnected),true,'event disclosure nodes survive refresh');
    await check(stageTrace).toHaveJSProperty('open',true);
    assert.equal(await stageTraceNode.evaluate(node=>node.isConnected),true,'stage trace disclosure survives refresh');
    await check(page.locator('#agents-history script,#agents-history img')).toHaveCount(0);
    assert.equal(await page.evaluate(()=>globalThis.__historyExecuted),undefined);
    const historyReadStart=relay.eventReads.length;
    relay.faults.denyNextEvents={task_id:historyTaskId,after_sequence:50};
    const deniedHistory=historyResponse(50);
    await page.locator('#agents-history-next').click();
    const deniedResponse=await deniedHistory;
    assert.equal(deniedResponse.status(),503);
    assert.equal(await deniedResponse.finished(),null);
    await check(page.locator('#agents-status')).toContainText('agent service is unavailable');
    await check(page.locator('#agents-history-range')).toHaveText('Events 1–50');
    await check(page.locator('#agents-history-next')).toBeEnabled();
    const retriedHistory=historyResponse(50);
    await page.locator('#agents-history-next').click();
    const retryResponse=await retriedHistory;
    assert.equal(retryResponse.status(),200);
    assert.equal(await retryResponse.finished(),null);
    await check(page.locator('#agents-history-range')).toHaveText('Events 51–61');
    await check(page.locator('#agents-history > li')).toHaveCount(11);
    await check(page.locator('#agents-history-next')).toBeDisabled();
    assert.deepEqual(relay.eventReads.slice(historyReadStart),[
      {request_id:deniedResponse.request().postDataJSON().request_id,task_id:historyTaskId,after_sequence:50,denied:true},
      {request_id:retryResponse.request().postDataJSON().request_id,task_id:historyTaskId,after_sequence:50,denied:false},
    ],'the selected page is denied once and retried at the same cursor');
    await page.locator('#agents-history-previous').click();
    await check(page.locator('#agents-history-range')).toHaveText('Events 1–50');
    await page.locator('#agents-history-next').click();
    await check(page.locator('#agents-history-range')).toHaveText('Events 51–61');
    await page.locator('#agents-history-first').click();
    await check(page.locator('#agents-history-range')).toHaveText('Events 1–50');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="queued"]')).toHaveAttribute('data-state','skipped');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="queued"] .fg-flow-detail')).toHaveText('Not recorded');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="running"]')).toHaveAttribute('data-state','completed');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[data-stage-id="running"] .fg-flow-detail')).toHaveText('#1–#61');
    await check(page.locator('#agents-stage-diagram .fg-flow-hops > li')).toHaveCount(1);
    await check(stageTrace).toHaveJSProperty('open',true);
    await stageTraceNode.dispose();
    for(const width of [320,390,1440]){
      await page.setViewportSize({width,height:844});
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'event metadata and summaries fit the viewport');
      assert.equal(await page.locator('#agents-stage-diagram .fg-flow-track').evaluate((node,phone)=>{
        const dir=getComputedStyle(node).flexDirection;
        return phone?dir==='column':dir==='row';
      },width<640),true,'stage track stacks vertically on phones and flows horizontally on desktop');
    }
    await page.setViewportSize({width:390,height:844});
    // Verify touch-target sizing, reduced-motion cascade, and direct tap/click toggle on phone without hover.
    assert.equal(await stageTrace.locator('summary').evaluate(node=>node.getBoundingClientRect().height>=44),true,'stage trace disclosure meets 44px touch target');
    await page.emulateMedia({reducedMotion:'reduce'});
    await check.poll(()=>page.locator('#agents-stage-diagram .fg-flow-node').first().evaluate(node=>getComputedStyle(node).transitionDuration.split(',').every(value=>Number.parseFloat(value)===0))).toBe(true);
    await check.poll(()=>stageTrace.locator(':scope > summary').evaluate(node=>getComputedStyle(node).transitionDuration)).toBe('0s');
    await check.poll(()=>stageTrace.locator(':scope > summary').evaluate(node=>getComputedStyle(node,'::after').transitionDuration)).toBe('0s');
    await page.emulateMedia({reducedMotion:'no-preference'});
    await stageTrace.locator('summary').click();
    await check(stageTrace).toHaveJSProperty('open',false);
    await stageTrace.locator('summary').click();
    await check(stageTrace).toHaveJSProperty('open',true);
    await check(stageTrace.locator('.fg-flow-hop-exact').first()).toBeVisible();
    await check(stageTrace.locator('.fg-flow-hop-exact').first()).toHaveText('2026-11-19T18:00:00Z');
    await page.locator('#agents-history-panel').scrollIntoViewIfNeeded();
    await screenshot(page,'ide-agents-event-history.png');
    await page.locator('#agents-back').click();
    relay.enableExternalWorkflow();
    await respondToLeaveWarning(() => page.reload({waitUntil:'domcontentloaded'}), 'leave');
    await page.locator('#agents-new').click();
    await page.locator('#agents-profile').selectOption('muse-db-primary');
    await check(page.locator('#agents-compose-help')).toContainText('advance this assignment automatically');
    await check(page.locator('#agents-assignment-policy')).toContainText('Each failure event allows up to 20 retries');
    await page.locator('#agents-prompt').fill('Run the external database workflow through independent review and verified merge.');
    await page.locator('#agents-criteria').fill('Preserve the envelope history and source receipts with no manual approvals.');
    await page.locator('#agents-send').click();
    await check(page.locator('#agents-workflow-phase')).toHaveText('Reviewing plan');
    await check(page.locator('#agents-stage-diagram .fg-flow-track-label')).toHaveText('Autonomous workflow · External Muse');
    await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText('Plan review');
    assert.deepEqual(
      await page.locator('#agents-stage-diagram .fg-flow-node .fg-flow-label').allTextContents(),
      ['Plan','Plan review','Implement','Code review','Checks','Merge','Merged'],
      'external Muse workflow renders Code review before Checks without fabricating Publish/Hosted',
    );
    const externalWorkflowTask=relay.latestWorkflowTaskId;
    relay.retryEvent(externalWorkflowTask,'compilation',20,{total:20});await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-retry-event')).toHaveText('Fixing compilation · Retry 20/20');
    relay.retryEvent(externalWorkflowTask,'tests',1,{total:21});await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-retry-event')).toHaveText('Fixing tests · Retry 1/20');
    await check(page.locator('#agents-workflow-revisions')).toContainText('Code revisions 21');
    relay.retryEvent(externalWorkflowTask,'tests',20,{total:40});await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-retry-event')).toHaveText('Fixing tests · Retry 20/20');
    await check(page.locator('#agents-workflow-revisions')).toContainText('Code revisions 40');
    relay.retryEvent(externalWorkflowTask,'tests',20,{status:'resolved',total:40});
    for(const [phase,label] of [['plan_revision','Revising plan'],['implement','Implementing'],['code_review','Reviewing code'],['checks','Running checks'],['merging','Merging'],['completed','Merged']]){
      relay.workflowPhase(externalWorkflowTask,phase);await page.locator('#agents-refresh').click();
      await check(page.locator('#agents-workflow-phase')).toHaveText(label);
      if(phase==='plan_revision'){
        await check(page.locator('#agents-stage-diagram .fg-flow-branch[data-branch-id="branch-plan-revision"]')).toBeVisible();
      }
      await check(page.locator('#agents-workflow-revisions')).toContainText('Code revisions 40');
      assert.equal((await page.locator('#agents-workflow-revisions').textContent()).includes('/20'),false,'lifetime revision totals are not an event budget');
      if(phase==='code_review'){
        await check(page.locator('#agents-workflow-reviewer')).toBeVisible();
        const linkedEvent=page.locator('#agents-history > li').filter({hasText:'Trusted fixture workflow: code_review.'});
        await check(linkedEvent).toHaveCount(1);
        await linkedEvent.locator('summary').click();
        await linkedEvent.locator('.fg-timeline-related').click();
        await check(page.locator('#agents-task-title')).toHaveText('Independent external workflow review');
        await check(page.locator('#agents-workflow-reviewer')).toBeHidden();
        await check(page.locator('#agents-stage-diagram .fg-flow-track-label')).toHaveText('Linked review task');
        assert.deepEqual(
          await page.locator('#agents-stage-diagram .fg-flow-node .fg-flow-label').allTextContents(),
          ['Queued','Running','Ready for review','Accepted'],
          'linked reviewer task renders its own review track instead of parent workflow phases',
        );
        await check(page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]')).toContainText('Ready for review');
        await page.locator('#agents-back').click();
        await check(page.locator('#agents-tasks')).not.toContainText('Independent external workflow review');
        await page.locator(`#agents-tasks [data-row-id="${externalWorkflowTask}"] .fg-row-select`).click();
        await check(page.locator('#agents-workflow-phase')).toHaveText(label);
      }
      for(const id of ['agents-accept','agents-resume','agents-plan-actions','agents-review-actions'])await check(page.locator('#'+id)).toBeHidden();
    }
    await check(page.locator('#agents-history')).toContainText('Trusted fixture workflow: completed.');
    await check(page.locator('#agents-workflow-source')).toContainText('Merged commit');
    for(const id of ['agents-progress-panel','agents-result-panel'])await check(page.locator('#'+id)).toBeHidden();
    await screenshot(page,'ide-agents-external-workflow.png');

    // Verify per-agent persistent branches on overview & interactive correlation CI/CD stages in reader.
    const branchActionId='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
    const mainActionId='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
    const cicdConsoleLog='[stage] compiling workspace\nerror[E0308]: mismatched types <script>window.__forgeCicdXss=true</script><img src=x onerror=window.__forgeCicdXss=true>\n'
      +'console detail line '.repeat(40)+'END_OF_CICD_LOG\n';
    relay.setCorrelationCicd(externalWorkflowTask,{
      branch_state:{
        project_alias:'workspace',
        agent_profile:'muse-db-primary',
        branch_name:'forge/agent/muse-db-primary',
        branch_scope:'branch/forge-agent-muse-db-primary-0123456789abcdef',
        head_commit_sha:'e'.repeat(40),
        fetched_main_sha:'f'.repeat(40),
        fetched_main_at:'2026-11-19T18:00:00Z',
        sync_status:'synced_with_main',
        conflict_paths:[],
        cache_status:'hit_branch',
        cache_updated_at:'2026-11-19T18:00:00Z',
        last_task_id:externalWorkflowTask,
        last_correlation_id:null,
        reservation_status:'active',
        active_scope_kind:'external_workflow',
        active_scope_id:'33333333-3333-4333-8333-333333333333',
        active_root_task_id:externalWorkflowTask,
        active_task_id:externalWorkflowTask,
        active_pr_url:'https://github.com/roachmaster/homelab/pull/101',
        reservation_epoch:2,
        reservation_blocker:null,
      },
      waiting_on_branch_owner:false,
      runs:[
        {
          run_id:branchActionId,action_id:branchActionId,job_name:'forge-native-verify',build_number:42,
          pipeline_scope:'branch',commit_sha:'e'.repeat(40),branch_name:'forge/agent/muse-db-primary',
          branch_scope:'branch/forge-agent-muse-db-primary-0123456789abcdef',
          phase:'complete',availability:'available',status:'failed',
          started_at:'2026-11-19T18:00:00Z',completed_at:'2026-11-19T18:01:00Z',
          stages:[
            {stage_id:'compile',label:'Compile',status:'passed',returncode:0,duration_ms:1200,error_count:0,has_console_log:true,log_byte_count:128,sub_phases:[]},
            {stage_id:'unit-tests',label:'Unit & In-Memory Tests',status:'failed',returncode:101,duration_ms:4300,tests_passed:964,tests_failed:1,tests_skipped:0,error_count:1,error_excerpt:'error[E0308]: mismatched types <script>window.__forgeCicdXss=true</script>',test_framework:'cargo-test',test_cases:[{name:'tests::failing_case',status:'failed'}],has_console_log:true,log_byte_count:cicdConsoleLog.length,sub_phases:[{phase_id:'all-features-unit-tests',phase_kind:'tests',label:'all-features-unit-tests',status:'failed',returncode:101,duration_ms:4300,tests_passed:964,tests_failed:1,tests_skipped:0}]},
          ],
        },
        {
          run_id:mainActionId,action_id:mainActionId,job_name:'forge-main-post-merge',build_number:43,
          pipeline_scope:'main',commit_sha:'f'.repeat(40),branch_name:'main',branch_scope:'main',
          phase:'complete',availability:'available',status:'passed',
          started_at:'2026-11-19T18:02:00Z',completed_at:'2026-11-19T18:04:00Z',
          stages:[
            {stage_id:'real-browser',label:'Real Browser (28 groups)',status:'passed',returncode:0,duration_ms:32000,tests_passed:28,tests_failed:0,tests_skipped:0,error_count:0,test_framework:'ui-qa',test_cases:[{name:'group-01-agents',status:'passed'}],has_console_log:false,log_byte_count:0,sub_phases:[]},
          ],
        },
      ],
    },{
      [`${branchActionId}:unit-tests:console`]:{content:cicdConsoleLog,pageSize:200,tests_passed:964,tests_failed:1,tests_skipped:0,test_cases:[{name:'tests::failing_case',status:'failed'}]},
    });
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-cicd-panel')).toBeVisible();
    await check(page.locator('#agents-cicd-branch')).toHaveText('forge/agent/muse-db-primary');
    await check(page.locator('#agents-cicd-sync')).toContainText('Synced with main');
    await check(page.locator('#agents-cicd-cache')).toContainText('Branch cache hit');
    await check(page.locator('#agents-cicd-owner')).toContainText('Owned by this task');
    // Latest run (Main CI #43) auto-selected with 28/28 tests.
    await check(page.locator('#agents-cicd-stages-diagram .fg-flow-dot-button[data-stage-id="real-browser"]')).toHaveAttribute('aria-pressed','true');
    await check(page.locator('#agents-cicd-stages-diagram .fg-flow-dot-button[data-stage-id="real-browser"]')).toContainText('28/28 tests');
    // Switch to Branch CI #42 run -> auto-selects failed Unit & In-Memory Tests dot.
    await page.locator(`#agents-cicd-runs [data-row-id="${branchActionId}"] .fg-row-select`).click();
    const unitDot=page.locator('#agents-cicd-stages-diagram .fg-flow-dot-button[data-stage-id="unit-tests"]');
    await check(unitDot).toHaveAttribute('aria-pressed','true');
    await check(page.locator('#agents-cicd-log-content')).toContainText('error[E0308]: mismatched types');
    await check(page.locator('#agents-cicd-panel script,#agents-cicd-panel img')).toHaveCount(0);
    assert.equal(await page.evaluate(()=>globalThis.__forgeCicdXss),undefined,'CI/CD stage output renders strictly as textContent');
    // Inspect Tests tab and Console paged log.
    await page.locator('#agents-cicd-tab-tests').click();
    await check(page.locator('#agents-cicd-tab-tests')).toHaveAttribute('aria-pressed','true');
    await check(page.locator('#agents-cicd-log-content')).toContainText('964 passed, 1 failed, 0 skipped');
    await page.locator('#agents-cicd-tab-console').click();
    await check(page.locator('#agents-cicd-log-content')).toContainText('[stage] compiling workspace');
    await check(page.locator('#agents-cicd-log-next')).toBeEnabled();
    await page.locator('#agents-cicd-log-next').click();
    await check(page.locator('#agents-cicd-log-prev')).toBeEnabled();
    // Keyboard select passing Compile dot and confirm focus survives refresh.
    const compileDot=page.locator('#agents-cicd-stages-diagram .fg-flow-dot-button[data-stage-id="compile"]');
    await compileDot.focus();await page.keyboard.press('Enter');
    await check(compileDot).toHaveAttribute('aria-pressed','true');
    await check(page.locator('#agents-cicd-log-status')).toContainText('0 errors');
    for(const width of [320,390,1440]){
      await page.setViewportSize({width,height:844});
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'CI/CD stage inspector fits viewport');
      assert.equal(await compileDot.evaluate(node=>node.getBoundingClientRect().height>=44),true,'interactive CI/CD stage dot meets 44px touch target');
    }
    await screenshot(page,'ide-agents-cicd-stages.png');
    await page.locator('#agents-back').click();
    await check(page.locator('#agents-branches-panel')).toBeVisible();
    await check(page.locator('#agents-branches-list')).toContainText('forge/agent/agy-primary');
    await check(page.locator('#agents-branches-list')).toContainText('Synced with main');

    assert(checkpoints >= 9, 'all task mutations and exact plan decisions checkpoint before IO');
    assert.equal(traffic.businessCalls().mail, 0, 'the complete Agents workflow sends no retired Mail requests');
    await assertNoRetiredMail(page);
    assert.deepEqual(errors, []);
    assert.equal(context.pages().length, 1, 'the Agents workflow uses one browser page');
  } finally {
    releaseAssignmentCatalog();
    releaseUsageOverview();
    try { if (context) await context.close(); }
    finally { try { if (fixture) await stopFixture(fixture); } finally { await agentsRelayOwner.close(); } }
  }
}

async function realJourney() {
  report.start('real.report.partial');
  await agentsJourney();
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
    await check(page.locator('#agents')).toBeVisible();
    await assertRetiredChat(page,traffic);
    assert.equal(traffic.count('resume'), 2, 'clean Forward reconnects once');
  });
  assert.deepEqual(errors, [], 'clean Back does not prompt and native draft warning only occurs when expected');
  assert.equal(traffic.count('pair'), 1, 'one explicit pairing request');
  await projectOptionsJourney(page, traffic);
  await retiredChatJourney(page, traffic);
  await inspect(page, 'workspace');
  await check(page.locator('#workspace-panel .fg-pane-body textarea:visible, #workbench [contenteditable="true"]:visible')).toHaveCount(0);
  await check(page.locator('#workbench-chat')).toHaveCount(0);
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
  await check(page.locator('#status')).toBeHidden();
  await check(page.locator('#workspace-panel-switch')).toBeDisabled();
  await check(page.locator('#problems-panel-switch')).toContainText('Problems (0)');
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
  await setExplorerMode(page, true);
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
  const input = page.locator('#project-query');
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
  // Repeated intents are rejected as Busy while the first request remains
  // pending. Its status copy can be superseded by that rejection; the model's
  // phase and admitted Cancel control establish the stable pending contract.
  await check(page.locator('#phase-label')).toHaveText('Inspecting');
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
  await sheet(page, 'project-options', true);
  await page.locator('#project').focus();
  await check(page.locator('#project')).toBeFocused();
  await check(page.locator('#status')).toContainText('Project ready.');
  await check(page.locator('#project')).toBeFocused();
  await sheet(page, 'project-options', false);
  report.pass('injected.requests.refresh_focus');
  report.start('injected.failures.unavailable_timeout');

  const reloadBookmark = checkpointSaved(page, snapshot => snapshot.workbench_open &&
    snapshot.workbench?.project === 'unavailable');
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
  await assertRetiredChat(page,traffic);
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
  await page.locator('.fg-header').click({ position: { x: 2, y: 2 } });
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
  // The expanded portal/provider journeys share this full-suite budget. Keep
  // individual assertions bounded and allow cleanup before the 300s supervisor.
  deadlineMs: 270_000,
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
    // user profile. Dormant workspace persistence uses the owned relay on loopback.
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
      ...(agentsRelayOwner ? [{ name: 'real_fixture', close: () => agentsRelayOwner.close() }] : []),
    ], currentReport);
  },
});
if (!passed) process.exitCode = 1;
