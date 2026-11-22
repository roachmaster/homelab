// Controlled runner-fixture contracts, not actual browser/WASM or production evidence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { startAgentsRelay } from './agents-relay.mjs';

async function implementationFixture(t) {
  const relay = await startAgentsRelay();
  t.after(() => relay.close());
  const token = await readFile(relay.tokenFile, 'utf8');
  let sequence = 0;
  async function post(kind, fields, headers = {}) {
    const request_id = 'verification-fixture-' + ++sequence;
    const response = await fetch('http://' + relay.endpoint + '/v1/dispatch', {
      method: 'POST', headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json',
        'Forge-Request-Id': request_id, 'Forge-Envelope-Version': '1', 'Forge-Contract-Version': '1',
        'Forge-Destination': 'ide.' + kind.replaceAll('_', '.'), ...headers },
      body: JSON.stringify({ kind, request_id, project_alias: 'workspace', ...fields }),
    });
    return { status: response.status, body: await response.json(), request_id };
  }
  const submitted = await post('work_submit', { base_revision: 'a'.repeat(40), mode: 'implement',
    agent_profile: 'agy-primary', prompt: 'Owned verification retry fixture',
    acceptance_criteria: 'Preserve the finished author', idempotency_key: 'fixture-assignment' });
  assert.equal(submitted.status, 202);
  const task_id = submitted.body.event.task_id;
  await post('work_get', { task_id });
  await post('work_events', { task_id });
  await post('work_events', { task_id });
  const before = relay.requireVerificationRetry(task_id);
  const request = { task_id, expected_revision: before.revision, review_id: before.review.review_id,
    decision: 'retry_infrastructure', agent_profile: 'agy-primary',
    note: 'Retry verification only; retain the finished author and spent rounds.',
    findings: [], verified_finding_ids: [], idempotency_key: 'fixture-verification-retry' };
  return { relay, post, before, request };
}

test('verification retry returns a new review and exposes it without creating another author', async t => {
  const { relay, post, before, request } = await implementationFixture(t);
  const tasks = relay.taskCount;
  const submissions = relay.calls.filter(kind => kind === 'work_submit').length;
  const retried = await post('work_review_decide', request);
  assert.equal(retried.status, 202);
  assert.equal(retried.body.protocol_version, 1);
  const ack = retried.body.event;
  assert.equal(ack.kind, 'work_review_decided');
  assert.equal(ack.request_id, retried.request_id);
  assert.equal(ack.project_alias, before.project_alias);
  assert.equal(ack.task_id, before.task_id);
  assert.equal(ack.base_revision, before.revision);
  assert.equal(ack.revision, before.revision + 1);
  assert.equal(ack.status, 'awaiting_review');
  assert.notEqual(ack.review_id, before.review.review_id);
  assert.match(ack.review_id, /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/);
  const opened = await post('work_get', { task_id: before.task_id });
  assert.equal(opened.body.event.task.review.review_id, ack.review_id);
  assert.equal(opened.body.event.task.revision, ack.revision);
  assert.equal(opened.body.event.task.attempt.attempt_id, before.attempt.attempt_id);
  assert.equal(opened.body.event.task.review.source_attempt_id, before.review.source_attempt_id);
  assert.equal(opened.body.event.task.review.round, before.review.round);
  assert.equal(opened.body.event.task.agent_profile, before.agent_profile);
  assert.equal(opened.body.event.task.mode, before.mode);
  const replay = await post('work_review_decide', request);
  assert.equal(replay.status, 202);
  assert.deepEqual(replay.body.event, { ...ack, request_id: replay.request_id });
  assert.equal(relay.verificationRetries.length, 1);
  assert.equal(relay.taskCount, tasks);
  assert.equal(relay.calls.filter(kind => kind === 'work_submit').length, submissions);
  const conflict = await post('work_review_decide', { ...request, note: 'A changed frozen intent' });
  assert.equal(conflict.status, 409);
  assert.equal(conflict.body.event.code, 'idempotency_conflict');
  assert.equal(relay.verificationRetries.length, 1);
});

test('verification retry keeps request correlation, project, revision and review guards', async t => {
  const { relay, post, before, request } = await implementationFixture(t);
  const tasks = relay.taskCount;
  for (const [changed, status] of [
    [{ project_alias: 'foreign' }, 403],
    [{ expected_revision: before.revision + 1 }, 409],
    [{ review_id: randomUUID() }, 409],
    [{ note: ' ' }, 400],
    [{ findings: [{ finding_id: 'unrelated' }] }, 400],
  ]) {
    assert.equal((await post('work_review_decide', { ...request, ...changed })).status, status);
    assert.equal(relay.verificationRetries.length, 0);
    assert.deepEqual(relay.reviewSnapshot(before.task_id), before);
  }
  for (const headers of [
    { 'Forge-Request-Id': 'foreign-request' },
    { 'Forge-Destination': 'ide.work.resume' },
  ]) {
    assert.equal((await post('work_review_decide', request, headers)).status, 503);
    assert.equal(relay.verificationRetries.length, 0);
  }
  assert.equal(relay.taskCount, tasks);
  assert.deepEqual(relay.reviewSnapshot(before.task_id), before);
});
