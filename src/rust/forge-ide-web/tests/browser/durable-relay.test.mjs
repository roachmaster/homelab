// Test-owned fixture contract, not production PostgreSQL or browser E2E evidence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { startDurableRelay } from './durable-relay.mjs';

test('inert workspace restore preserves full saved Chat metadata without hydration', async t => {
  const relay = await startDurableRelay('', '');
  t.after(() => relay.close());
  const token = await readFile(relay.tokenFile, 'utf8');
  const tab = 'a'.repeat(32);
  async function post(body) {
    const response = await fetch('http://' + relay.endpoint + '/v1/workspace', {
      method: 'POST', headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' },
      body: JSON.stringify({ tab, ...body }),
    });
    return { status: response.status, body: await response.json() };
  }
  const first = await post({ operation: 'restore', preserve_chat: true });
  assert.equal(first.status, 200);
  assert.equal(first.body.snapshot, null);
  const snapshot = {
    chat: { selected: 0, model_id: 'fixture-model',
      conversations: [{ draft: 'Preserved draft', chat_id: '11111111-1111-4111-8111-111111111111', revision: 4,
        messages: [{role:'user',content:'Saved question'},{role:'assistant',content:'Saved answer'}] }],
      pending_run: { chat_id: '11111111-1111-4111-8111-111111111111',
        idempotency_key: '22222222-2222-4222-8222-222222222222',
        run_id: '33333333-3333-4333-8333-333333333333', expected_revision: 4, model_id: 'fixture-model' } },
    workbench: null, workbench_open: false,
  };
  relay.seedWorkspace(tab, snapshot);
  const restored = await post({ operation: 'restore', preserve_chat: true });
  assert.equal(restored.status, 200);
  assert.deepEqual(restored.body.snapshot, snapshot);
  const saved = await post({ operation: 'save', lease: restored.body.lease, revision: 1, snapshot });
  assert.equal(saved.status, 200);
  assert.deepEqual(relay.workspaceSnapshot(tab), snapshot);
  assert.equal(relay.submitted(), 0);
  assert.equal(relay.completions(), 0);
  for (const forbidden of [{history:true}, {models:true}, {history:true,models:true}]) {
    assert.equal((await post({ operation: 'restore', preserve_chat: true, ...forbidden })).status, 400,
      'inert restore cannot also request linked Chat capability hydration');
    assert.deepEqual(relay.workspaceSnapshot(tab), snapshot);
  }
});
