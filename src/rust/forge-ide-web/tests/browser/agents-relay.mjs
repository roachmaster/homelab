// Owned deterministic runner stand-in; never invokes a real coding agent.
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const time = '2026-11-19T18:00:00Z';
const project = 'workspace';
const base = 'a'.repeat(40);
const resultText = '## Fixture agent result\n\nVerified first section.\n\n'
  + 'A readable fixture paragraph with élève and safe <script> text.\n\n'.repeat(1500)
  + '\nFinal fixture marker.\n';
const bytes = Buffer.from(resultText);
const digest = createHash('sha256').update(bytes).digest('hex');
const profiles = [
  { alias: 'agy-primary', label: 'Gemini', backend: 'antigravity', model: 'Gemini fixture', modes: ['plan', 'review'] },
  { alias: 'codex-primary', label: 'Codex', backend: 'codex', model: 'Codex fixture', modes: ['plan', 'review'] },
];

function task(status, number, input = {}) {
  const taskId = number === 0 ? '11111111-1111-4111-8111-111111111111' : randomUUID();
  return {
    task_id: taskId, project_alias: project, base_revision: base, mode: input.mode ?? 'review',
    agent_profile: input.agent_profile ?? 'agy-primary', recipe_digest: 'b'.repeat(64),
    status, revision: status === 'awaiting_review' ? 3 : 1,
    created_at: `2026-11-19T18:00:${String(number).padStart(2, '0')}Z`,
    title: input.prompt ? Array.from(input.prompt.split('\n')[0]).slice(0, 128).join('').replace(/[\r\t]/g, ' ') : 'Fixture existing report',
    prompt: input.prompt ?? 'Review the fixture', acceptance_criteria: input.acceptance_criteria ?? 'Return the complete report',
    attempt_id: randomUUID(), sequence: 1,
  };
}
function summary(t) {
  return Object.fromEntries(['task_id', 'project_alias', 'base_revision', 'mode', 'agent_profile', 'recipe_digest', 'status', 'revision', 'created_at', 'title'].map(k => [k, t[k]]));
}
function detail(t) {
  const processing = ['running', 'cancel_requested'].includes(t.status);
  return {
    ...summary(t), prompt: t.prompt, acceptance_criteria: t.acceptance_criteria,
    attempt: t.status === 'queued' ? null : { attempt_id: t.attempt_id, worker_id: 'fixture-worker', phase: processing ? 'running' : 'stopped', started_at: time, finished_at: processing ? null : time, summary: processing ? 'The fixture agent is working.' : 'Fixture process scope stopped.' },
    result: ['awaiting_review', 'completed'].includes(t.status) ? { attempt_id: t.attempt_id, sha256: digest, byte_count: bytes.length, chunk_count: Math.ceil(bytes.length / 16384) } : null,
    reconciliation_receipt_id: null,
  };
}
function usage(alias, tasks) {
  const p = profiles.find(item => item.alias === alias);
  const active = [...tasks.values()].find(t => t.agent_profile === alias && ['running', 'cancel_requested'].includes(t.status));
  const codex = p.backend === 'codex';
  return {
    alias, label: p.label, provider: p.backend, model: p.model, worker_state: active ? 'running' : 'idle', active_task_id: active?.task_id ?? null,
    snapshot_state: codex ? 'fresh' : 'unknown', observed_at: codex ? time : null, source: 'owned-browser-fixture', version: '1', last_error: null, refresh_pending: false,
    allowance: { state: codex ? 'available' : 'unsupported', reason: codex ? null : 'Headless allowance unavailable', ordinary_usage_allowed: codex ? true : null,
      buckets: codex ? [{ key: 'fixture-window', label: 'Provider window', used_percent: 12.5, window_minutes: 90, resets_at: '2026-11-19T19:30:00Z' }] : [] },
    jobs: { scope: 'forge_project', input_tokens: codex ? '9007199254740993' : null, output_tokens: codex ? '5' : null, cached_input_tokens: codex ? '2' : null, total_tokens: codex ? '9007199254740998' : null, known_attempts: codex ? 1 : 0, unknown_attempts: codex ? 0 : 1, elapsed_ms: codex ? '1000' : null },
    account_activity: { state: 'unknown', lifetime_tokens: null, daily: [] },
  };
}

export async function startAgentsRelay() {
  const directory = await mkdtemp(join(tmpdir(), 'forge-agents-ui-'));
  const tokenFile = join(directory, 'relay-token');
  const token = randomBytes(32).toString('hex');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const seed = task('awaiting_review', 0);
  const tasks = new Map([[seed.task_id, seed]]);
  const receipts = new Map();
  const controls = new Map();
  const calls = [];
  const faults = { dropNextSubmit: false, denyNextSubmit: false };
  let refreshes = 0;
  const server = createServer(async (request, response) => {
    try {
      if (request.method !== 'POST' || request.url !== '/v1/dispatch' || request.headers.authorization !== `Bearer ${token}`) {
        response.writeHead(401, { 'Content-Type': 'application/json' }); response.end('{}'); return;
      }
      const chunks = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; if (size > 16384) throw new Error('fixture body bound'); chunks.push(chunk); }
      const input = JSON.parse(Buffer.concat(chunks));
      const requestId = input.request_id;
      if (request.headers['forge-request-id'] !== requestId || request.headers['forge-envelope-version'] !== '1' || request.headers['forge-contract-version'] !== '1') throw new Error('fixture headers');
      calls.push(input.kind);
      const send = (status, kind, fields = {}) => { response.writeHead(status, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ protocol_version: 1, event: { kind, request_id: requestId, ...fields } })); };
      const failed = (status, code) => send(status, input.kind.startsWith('usage_') ? 'usage_failed' : 'work_failed', { code });
      if (input.kind === 'work_catalog') {
        send(200, 'work_catalog', { projects: [{ alias: project, label: 'Fixture workspace', base_revision: base }], profiles, worker_status: 'ready', observed_at: time }); return;
      }
      if (input.project_alias !== project) { failed(403, 'denied'); return; }
      if (input.kind === 'usage_overview' || input.kind === 'usage_detail') {
        const selected = input.kind === 'usage_detail' ? profiles.filter(p => p.alias === input.agent_profile) : profiles;
        send(200, input.kind, { project_alias: project, providers: selected.map(p => usage(p.alias, tasks)) }); return;
      }
      if (input.kind === 'usage_refresh') {
        if (!profiles.some(p => p.alias === input.agent_profile)) { failed(403, 'denied'); return; }
        refreshes++; send(202, 'usage_refresh_queued', { project_alias: project, refresh_id: randomUUID(), profiles: [input.agent_profile], coalesced: false }); return;
      }
      if (input.kind === 'work_list') {
        const ordered = [...tasks.values()].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.task_id.localeCompare(b.task_id));
        const after = input.cursor ? ordered.findIndex(t => t.task_id === input.cursor) + 1 : 0;
        const available = ordered.slice(after).filter(t => !input.status_filter || t.status === input.status_filter);
        const page = available.slice(0, input.limit ?? 20);
        send(200, 'work_listed', { project_alias: project, tasks: page.map(summary), next_cursor: available.length > page.length ? page.at(-1).task_id : null }); return;
      }
      if (input.kind === 'work_submit') {
        if (faults.denyNextSubmit) { faults.denyNextSubmit = false; failed(403, 'denied'); return; }
        let receipt = receipts.get(input.idempotency_key);
        if (!receipt) {
          const next = task('queued', tasks.size, input); tasks.set(next.task_id, next);
          receipt = { task_id: next.task_id, project_alias: project, revision: 1, status: 'queued', recipe_digest: next.recipe_digest };
          receipts.set(input.idempotency_key, receipt);
        }
        if (faults.dropNextSubmit) { faults.dropNextSubmit = false; response.destroy(); return; }
        send(202, 'work_submitted', receipt); return;
      }
      const selected = tasks.get(input.task_id);
      if (!selected) { failed(404, 'not_found'); return; }
      if (input.kind === 'work_get') {
        if (selected.status === 'queued') { selected.status = 'running'; selected.revision++; selected.sequence = 4; }
        send(200, 'work_opened', { task: detail(selected) }); return;
      }
      if (input.kind === 'work_events') {
        if (selected.status === 'running') { selected.status = 'awaiting_review'; selected.revision++; selected.sequence = 9; }
        else if (selected.status === 'cancel_requested') { selected.status = 'cancelled'; selected.revision++; selected.sequence = 11; }
        const events = selected.sequence > (input.after_sequence ?? 0) ? [{ sequence: selected.sequence, kind: selected.status === 'awaiting_review' ? 'awaiting_review' : selected.status, created_at: time }] : [];
        send(200, 'work_events', { project_alias: project, task_id: selected.task_id, revision: selected.revision, status: selected.status, events, next_sequence: events.at(-1)?.sequence ?? input.after_sequence ?? 0 }); return;
      }
      if (input.kind === 'work_cancel' || input.kind === 'work_accept') {
        let receipt = controls.get(input.idempotency_key);
        if (!receipt) {
          if (input.expected_revision !== selected.revision) { failed(409, 'revision_conflict'); return; }
          const before = selected.revision;
          if (input.kind === 'work_accept' && selected.status !== 'awaiting_review') { failed(409, 'not_reviewable'); return; }
          selected.status = input.kind === 'work_accept' ? 'completed' : selected.status === 'queued' ? 'cancelled' : 'cancel_requested'; selected.revision++;
          receipt = { project_alias: project, task_id: selected.task_id, base_revision: before, revision: selected.revision, status: selected.status };
          controls.set(input.idempotency_key, receipt);
        }
        send(200, input.kind === 'work_accept' ? 'work_accepted' : 'work_cancel_recorded', receipt); return;
      }
      if (input.kind === 'work_result') {
        if (!['awaiting_review', 'completed'].includes(selected.status) || input.attempt_id !== selected.attempt_id) { failed(404, 'not_found'); return; }
        const [cursorDigest, rawOffset] = input.cursor?.split('.') ?? [digest, '0'];
        const offset = Number(rawOffset);
        if (cursorDigest !== digest || !Number.isSafeInteger(offset) || offset < 0 || offset > bytes.length) { failed(409, 'result_mismatch'); return; }
        let end = Math.min(bytes.length, offset + (input.limit_bytes ?? 16384));
        while (end < bytes.length && (bytes[end] & 0xc0) === 0x80) end--;
        send(200, 'work_result_page', { project_alias: project, task_id: selected.task_id, attempt_id: selected.attempt_id, result_digest: digest, content: bytes.subarray(offset, end).toString('utf8'), offset_bytes: offset, next_cursor: end === bytes.length ? null : `${digest}.${end}`, complete: end === bytes.length, total_bytes: bytes.length }); return;
      }
      failed(404, 'not_found');
    } catch (_) { if (!response.destroyed) { response.writeHead(503, { 'Content-Type': 'application/json' }); response.end('{}'); } }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return { endpoint: `127.0.0.1:${server.address().port}`, tokenFile, faults, calls,
    get taskCount() { return tasks.size; }, get refreshes() { return refreshes; },
    async close() { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); await rm(directory, { recursive: true, force: true }); },
  };
}
