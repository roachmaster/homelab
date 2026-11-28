// Owned browser acceptance, never application glue or a replacement WASM model.
import assert from 'node:assert/strict';

const mutations = new Set(['work_submit', 'work_cancel', 'work_accept', 'work_resume',
  'work_plan_decide', 'work_review_decide', 'work_repair', 'work_publication_retry']);

async function position(page, row) {
  return row.evaluate(node => ({
    top: node.getBoundingClientRect().top,
    page: document.scrollingElement.scrollTop,
    agents: document.querySelector('#agents').scrollTop,
  }));
}

async function samePosition(check, page, row, before) {
  await check.poll(async () => {
    const after = await position(page, row);
    return ['top', 'page', 'agents'].every(key => Math.abs(after[key] - before[key]) <= 1);
  }).toBe(true);
}

async function top(page) {
  await page.evaluate(() => {
    document.scrollingElement.scrollTop = 0;
    document.querySelector('#agents').scrollTop = 0;
  });
}

async function disclosure(page, check, id, open) {
  const details = page.locator('#' + id);
  if (await details.evaluate(node => node.open) !== open) {
    await details.locator(':scope > summary').focus();
    await page.keyboard.press('Enter');
  }
  await check(details).toHaveJSProperty('open', open);
}

async function hiddenExisting(page, check, ids) {
  for (const id of ids) {
    const control = page.locator('#' + id);
    await check(control).toHaveCount(1);
    await check(control).toBeHidden();
  }
}

export async function exerciseAgentsTriage({ page, relay, check, screenshot, origin }) {
  const scope = page.locator('#agents-task-scope');
  const attention = page.locator('#agents-attention');
  const rows = page.locator('#agents-tasks .fg-tree-row');
  const row = id => page.locator(`#agents-tasks [data-row-id="${id}"]`);
  const select = id => row(id).locator('.fg-row-select');
  const now = page.locator('#agents-triage-now');
  const kind = page.locator('#agents-triage-kind');
  const next = page.locator('#agents-triage-next');
  const mutationCount = () => relay.calls.filter(call => mutations.has(call)).length;
  const mutationsBefore = mutationCount();
  let releaseRead = () => {};
  let held = false;
  let holdTask;
  const readGate = new Promise(resolve => { releaseRead = resolve; });
  const holdRead = async route => {
    const request = route.request().postDataJSON();
    if (!held && request.kind === 'work_get' && request.task_id === holdTask) {
      held = true;
      await readGate;
    }
    await route.fallback();
  };
  await page.route(origin + '/forge/v1/dispatch', holdRead);
  try {
    await page.locator('#open-agents').focus();
    await page.keyboard.press('Enter');
    for (const id of ['agents-triage', 'agents-triage-now', 'agents-triage-why',
      'agents-triage-next', 'agents-triage-kind', 'agents-attention', 'agents-current-stage',
      'agents-stage-evidence', 'agents-task-technical', 'agents-workflow', 'agents-history-panel']) {
      await check(page.locator('#' + id)).toHaveCount(1);
    }
    await check(scope).toHaveValue('active');
    await check(page.locator('#agents-tasks')).toContainText('No active assignments');
    await check(rows).toHaveCount(0);
    await check(attention).toBeVisible();
    await check(attention).toHaveAttribute('aria-pressed', 'false');
    await check(page.locator('#agents-worker')).toHaveText('Worker ready');

    const corpus = relay.seedTriage();
    await page.locator('#agents-refresh').click();
    await check(row(corpus.automatic)).toBeVisible();
    await check(scope.locator('option')).toHaveText([
      /^Active \(\d+\)$/, /^Needs attention \(\d+\)$/, /^History \(\d+\)$/, /^All assignments \(\d+\)$/,
    ]);
    await check(row(corpus.automatic).locator('.fg-row-summary .fg-badge')).toHaveText('Revising plan');
    await check(row(corpus.boundary).locator('.fg-row-summary .fg-badge')).toHaveText('Revising plan');
    await check(row(corpus.boundary)).toContainText('20 of 20');
    await check(row(corpus.automatic)).toContainText('1 of 20');
    await check(row(corpus.automatic)).toContainText('Gemini');
    await check(row(corpus.automatic).locator('time')).toHaveAttribute('datetime', /^2026-11-23T/);
    const title = (await row(corpus.automatic).locator('.fg-row-label').textContent()).replace(/…$/, '');
    assert(corpus.prompt.startsWith(title), 'the row title remains an exact prefix of the supplied assignment');
    assert(/\s/.test(corpus.prompt[title.length]), 'the bounded title ends at a whole-word boundary');

    for (const width of [1363, 320, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await top(page);
      await check(attention).toBeVisible();
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
        'the triage inbox has no horizontal page overflow');
      const attentionBox = await attention.boundingBox();
      assert(attentionBox && attentionBox.width >= 44 && attentionBox.height >= 44, 'attention is a touch-sized explicit scope');
      await screenshot(page, `ide-agents-triage-inbox-${width}.png`);
    }

    // A changed badge reaches the actual WASM by ordinary polling. Keeping the
    // focusable node and its geometry also protects against replacement-on-refresh.
    await page.setViewportSize({ width: 390, height: 844 });
    const focusedId = corpus.rows[3];
    const focused = select(focusedId);
    await focused.focus();
    await check(focused).toBeFocused();
    const focusedNode = await focused.elementHandle();
    const before = await position(page, focused);
    assert(before.agents > 0 || before.page > 0, 'the test exercises a scrolled list');
    relay.updateTriagePhase(focusedId, 'implement');
    await check(row(focusedId).locator('.fg-row-summary .fg-badge')).toHaveText('Implementing');
    await check(focused).toBeFocused();
    assert.equal(await focusedNode.evaluate(node => node.isConnected), true, 'background refresh retains the focused DOM node');
    await samePosition(check, page, focused, before);
    await page.keyboard.press('Enter');
    await check(now).toHaveText('Implementing');
    await check(page.locator('#agents-reader')).toBeVisible();
    await page.locator('#agents-back').focus();
    await page.keyboard.press('Enter');
    await check(focused).toBeFocused();
    await check(focused).toHaveAttribute('aria-selected', 'true');
    await samePosition(check, page, focused, before);
    await focusedNode.dispose();

    // Failed refresh keeps accepted rows and announces failure, not an empty
    // successful scope or a fabricated task transition.
    relay.faults.denyNextList = true;
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-status')).toContainText('unavailable');
    await check(row(focusedId)).toBeVisible();
    await check(row(focusedId).locator('.fg-row-summary .fg-badge')).toHaveText('Implementing');
    await page.locator('#agents-refresh').click();
    await check(page.locator('#agents-refresh')).toBeEnabled();

    // Delay an actual read, then fail a different read: neither can inherit the
    // last selected task's progress or imply a successful new assignment.
    holdTask = corpus.automatic;
    await select(holdTask).click();
    await check.poll(() => held).toBe(true);
    await check(now).toContainText('Loading');
    await check(kind).not.toHaveText('Complete');
    await hiddenExisting(page, check, ['agents-stage-evidence']);
    releaseRead();
    await check(now).toHaveText('Revising plan');
    await check(kind).toHaveText('Automatic retry');
    await check(next).toContainText('review');
    await check(page.locator('#agents-retry-event')).toHaveText('Plan review retry budget · 1 of 20 used · Open');
    await disclosure(page, check, 'agents-workflow', true);
    await check(page.locator('#agents-workflow-revisions')).toContainText('Plan revisions 7');
    await check(page.locator('#agents-workflow-revisions')).not.toContainText('/20');
    await disclosure(page, check, 'agents-workflow', false);
    await disclosure(page, check, 'agents-assignment-details', true);
    await check(page.locator('#agents-task-prompt')).toHaveText(corpus.prompt);
    await disclosure(page, check, 'agents-assignment-details', false);
    for (const width of [1363, 320, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await top(page);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
        'Now / Why / Next and the compact current stage fit the viewport');
      const nowBox = await now.boundingBox();
      assert(nowBox && nowBox.y >= 0 && nowBox.y + nowBox.height <= 844, 'the authoritative current state is visible above the fold');
      await check(page.locator('#agents-current-stage')).toBeVisible();
      await screenshot(page, `ide-agents-triage-detail-${width}.png`);
      await disclosure(page, check, 'agents-stage-evidence', true);
      const stage = page.locator('#agents-stage-diagram .fg-flow-node[aria-current="step"]');
      await check(stage).toHaveCount(1);
      await stage.scrollIntoViewIfNeeded();
      const stageBox = await stage.boundingBox();
      assert(stageBox && stageBox.x >= 0 && stageBox.x + stageBox.width <= width + 1,
        'the full current stage is reachable at desktop and narrow widths');
      assert.equal(await page.locator('#agents-stage-diagram .fg-flow-track').evaluate(node =>
        getComputedStyle(node).flexDirection === 'column'), true, 'the full stage list has a deliberate vertical reading order');
      await disclosure(page, check, 'agents-stage-evidence', false);
    }
    await page.locator('#agents-back').click();
    relay.faults.denyNextGet = corpus.rows[2];
    await select(corpus.rows[2]).click();
    await check(kind).toHaveText('Unavailable');
    await check(now).not.toHaveText('Revising plan');
    await hiddenExisting(page, check, ['agents-stage-evidence']);
    await check(next).toContainText('Refresh');
    await page.locator('#agents-back').click();

    // Scope is server-owned: the cancelled author is still escalated; an open
    // automatic retry remains Active and does not acquire an operator action.
    await check(attention).toBeEnabled();
    await attention.focus();
    await page.keyboard.press('Enter');
    await check(scope).toHaveValue('attention');
    await check(attention).toHaveAttribute('aria-pressed', 'true');
    await check(row(corpus.escalation)).toBeVisible();
    await check(row(corpus.automatic)).toHaveCount(0);
    await check(row(corpus.boundary)).toHaveCount(0);
    await check(row(corpus.earlyEscalation).locator('.fg-row-summary .fg-badge')).toHaveText('Needs attention');
    await check(row(corpus.earlyEscalation)).toContainText('1 of 20');
    await check(row(corpus.escalation).locator('.fg-row-summary .fg-badge')).toHaveText('Needs attention');
    await select(corpus.escalation).focus();
    await page.keyboard.press('Enter');
    await check(now).toHaveText('Needs attention');
    await check(kind).toHaveText('Your action');
    await check(page.locator('#agents-triage-why')).toContainText('saved plan is preserved');
    await check(page.locator('#agents-retry-event')).toContainText('20 of 20 used');
    await hiddenExisting(page, check, ['agents-accept', 'agents-plan-actions', 'agents-verification-actions',
      'agents-review-approve', 'agents-review-repair', 'agents-review-escalate', 'agents-review-retry']);
    await top(page);
    await screenshot(page, 'ide-agents-triage-attention-390.png');
    await page.locator('#agents-back').click();
    await check(select(corpus.escalation)).toBeFocused();
    await check(scope).toHaveValue('attention');

    await select(corpus.manual).click();
    await check(now).toHaveText('Plan ready for your review');
    await check(kind).toHaveText('Your action');
    await check(next).toContainText('Resume');
    for (const id of ['agents-plan-resume', 'agents-plan-revise', 'agents-plan-cancel']) {
      await check(page.locator('#' + id)).toHaveCount(1);
      await check(page.locator('#' + id)).toBeVisible();
    }
    await check(page.locator('#agents-plan-resume')).toBeEnabled();
    await check(page.locator('#agents-plan-revise')).toBeDisabled();
    await check(page.locator('#agents-plan-cancel')).toBeEnabled();
    await hiddenExisting(page, check, ['agents-accept']);
    await page.locator('#agents-back').click();

    await scope.selectOption('active');
    await check(row(corpus.muse)).toBeVisible();
    await select(corpus.muse).click();
    await check(now).toHaveText('Reviewing plan');
    await check(kind).toHaveText('Review');
    await check(next).toContainText('review');
    await check(page.locator('#agents-external-result h1')).toHaveText('Owned Muse triage plan');
    await hiddenExisting(page, check, ['agents-accept', 'agents-resume', 'agents-plan-actions', 'agents-verification-actions',
      'agents-review-approve', 'agents-review-repair', 'agents-review-escalate', 'agents-review-retry']);
    await top(page);
    await screenshot(page, 'ide-agents-triage-muse-390.png');
    await page.locator('#agents-back').click();
    await select(corpus.delivery).click();
    await check(now).toHaveText('Main verification queued');
    await check(page.locator('#agents-worker')).toHaveText('Worker ready');
    await check(kind).not.toHaveText('Complete');
    await check(page.locator('#agents-delivery-state')).toHaveText('Main verification queued');
    await page.locator('#agents-back').click();
    const defectId = relay.seedDefects(corpus.automatic);
    await page.locator('#agents-refresh').click();
    const kindFilter = page.locator('#agents-task-kind');
    await kindFilter.selectOption('defect');
    await check(rows).toHaveCount(1);
    await check(row(defectId)).toContainText('Defect');
    await check(row(defectId)).toContainText('high');
    for (const viewport of [{width:1440,height:1000},{width:390,height:844}]) {
      await page.setViewportSize(viewport);
      await check(kindFilter).toBeVisible();
      const bounds=await kindFilter.boundingBox();
      assert(bounds && bounds.height >= 44 && bounds.width >= 44, 'kind filter is touch sized');
      await select(defectId).focus();
      await page.keyboard.press('Enter');
      await check(page.locator('#agents-defect-badge')).toHaveText('Defect · high');
      await check(page.locator('#agents-defect-context')).toContainText('Failure remains visible');
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth), 'defect reader fits viewport');
      await top(page);
      await screenshot(page, `ide-agents-defect-${viewport.width}.png`);
      await page.locator('#agents-defect-discovery').focus();
      await page.keyboard.press('Enter');
      await check(page.locator('#agents-filed-defects')).toContainText('Report parser loses an observed failure');
      await page.locator(`#agents-filed-defects [data-row-id="${defectId}"] .fg-row-select`).click();
      await check(page.locator('#agents-defect-badge')).toBeVisible();
      await page.locator('#agents-back').click();
    }
    await kindFilter.selectOption('feature');
    await check(row(defectId)).toHaveCount(0);
    await kindFilter.selectOption('all');
    await check(row(defectId)).toBeVisible();
    assert.equal(mutationCount(), mutationsBefore, 'triage navigation and evidence reads never mutate any assignment');
  } finally {
    releaseRead();
    await page.unroute(origin + '/forge/v1/dispatch', holdRead);
  }
}
