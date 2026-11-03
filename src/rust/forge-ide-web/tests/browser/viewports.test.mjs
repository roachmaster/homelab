import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { VIEWPORTS, verifyViewportProfiles } from './viewports.mjs';
import { SCENARIOS } from './catalog.mjs';

test('ten immutable viewport cases cover five portrait/landscape pairs and both breakpoints', () => {
  assert.equal(VIEWPORTS.length, 10);
  assert.equal(new Set(VIEWPORTS.map(item => item.id)).size, 10);
  assert.equal(new Set(VIEWPORTS.map(item => item.screenshot)).size, 10);
  assert.ok(Object.isFrozen(VIEWPORTS));
  for (const item of VIEWPORTS) {
    assert.ok(Object.isFrozen(item));
    assert.match(item.id, /^real\.viewport\.[a-z0-9-]+-(portrait|landscape)$/);
    assert.match(item.screenshot, /^ide-viewport-[a-z0-9-]+\.png$/);
    const scenario = SCENARIOS.find(scenario => scenario.id === item.id);
    assert.deepEqual(scenario.viewport, item);
    assert.ok(scenario.title.includes(`${item.width}×${item.height}`));
    assert.equal(scenario.backend, 'real_mtls');
  }
  assert.equal(VIEWPORTS.filter(item => item.width <= 800).length, 6);
  assert.equal(VIEWPORTS.filter(item => item.width > 800).length, 4);
  assert.deepEqual(VIEWPORTS.find(item => item.profile === 'iPhone 13 landscape').width, 750);
});

test('profile verification fails closed on missing or changed pinned registry dimensions', () => {
  const devices = Object.fromEntries(VIEWPORTS.map(item => [item.profile, { viewport: { width: item.width, height: item.height } }]));
  assert.doesNotThrow(() => verifyViewportProfiles(devices));
  assert.throws(() => verifyViewportProfiles({}), /registry mismatch/);
  devices['iPhone 13 landscape'].viewport.width = 664;
  assert.throws(() => verifyViewportProfiles(devices), /registry mismatch/);
});

test('workflow explicitly allowlists each fixture screenshot without a broad PNG glob', async () => {
  const workflow = await readFile(new URL('../../../../../.github/workflows/ide-ui-browser.yml', import.meta.url), 'utf8');
  for (const item of VIEWPORTS) assert.ok(workflow.includes(`/browser/${item.screenshot}`));
  assert.equal(workflow.includes('/browser/*.png'), false);
});
