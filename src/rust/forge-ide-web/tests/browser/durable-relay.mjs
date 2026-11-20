// Controlled localhost relay. Browser requests still use the generated Worker,
// real WASM and native host. The in-process store stands in for the runner here.
import { randomBytes, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const maxBytes = 2 * 1024 * 1024;
export const mailSubject = 'Review ' + 'unbrokenSubject'.repeat(7);
export const mailPreview = 'Subject: Fixture review. The mail reader preserves a useful preview, clear sender details, and the original message order. Please review the workspace and reply with your findings.';

function mailThread(archived, includeMetadata = false) {
  return {
    thread_id: archived ? '56ad75f0-8cae-4766-b271-57620606c92d' : '8452b258-df30-4985-bfa1-cd98245b2a06',
    subject: archived ? 'Archived fixture thread' : mailSubject,
    status: archived ? 'archived' : 'ready',
    updated_at: archived ? '2026-11-17T01:23:54.993456+00:00' : '2026-11-16T22:11:59.293456Z',
    turn_count: 1,
    revision: 2,
    latest_run_id: 'b9b570aa-775d-4c25-b4ac-c122e292e2c3',
    archive_reason: archived ? 'context_limit' : null,
    ...(includeMetadata ? { preview: mailPreview } : {}),
  };
}

async function readJson(request) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > maxBytes) throw new Error('Fixture relay request too large');
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks));
}

export async function startDurableRelay(prompt, reply) {
  const directory = await mkdtemp(join(tmpdir(), 'forge-ui-durable-'));
  const tokenFile = join(directory, 'relay-token');
  const token = randomBytes(32).toString('hex');
  const tabs = new Map();
  const history = new Map();
  const removed = new Set();
  const statefulThreads = [mailThread(false, true), mailThread(true, true)];
  // Message dates belong to the saved messages, independent of later archival.
  const mailMessageDatesByThread = Object.freeze(Object.fromEntries(statefulThreads.map(thread => [
    thread.thread_id, Object.freeze(thread.status === 'archived'
      ? ['2026-11-17T01:22:32Z', '2026-11-17T01:23:54Z']
      : ['2026-11-16T22:10:32Z', '2026-11-16T22:11:59Z']),
  ])));
  const runs = new Map();
  const submissions = new Map();
  const workspaceRestores = [];
  let submitted = 0;
  let completions = 0;
  let dropFirstAcknowledgement = true;
  let loadedModel = 'gpt-oss-20b';
  function finish() {
    const run = [...runs.values()].find(run => run.status === 'queued' && run.content === prompt);
    if (!run) throw new Error('Fixture chat is not queued');
    run.status = 'completed';
    loadedModel = run.model_id;
    run.events.push({ sequence: 2, kind: 'completed' });
    completions++;
    history.set(run.chat_id, {
      summary: {
        chat_id: run.chat_id, title: prompt, updated_at: '2026-11-16T12:00:00Z', revision: 1,
        retention: { kind: 'expires', expires_on: '2026-12-16', time_zone: 'UTC', expires_at: '2026-12-17T00:00:00Z' },
      },
      messages: [
        { role: 'user', content: prompt, position: 1 },
        { role: 'assistant', content: reply, position: 2 },
      ],
    });
  }
  const server = createServer({ requestTimeout: 5000 }, async (request, response) => {
    const send = (status, body) => {
      response.writeHead(status, { 'Content-Type': 'application/json', Connection: 'close' });
      response.end(JSON.stringify(body));
    };
    if (request.method !== 'POST' || request.headers.authorization !== `Bearer ${token}`) {
      send(401, {});
      return;
    }
    try {
      const body = await readJson(request);
      if (request.url === '/v1/dispatch') {
        const id = body.request_id;
        const replyTo = (status, event) => send(status, { protocol_version: 1, event: { request_id: id, ...event } });
        const destination = request.headers['forge-destination'];
        const fail = (status, code) => replyTo(status, { kind: destination.startsWith('ide.chat.run.') ? 'chat_run_failed' : 'chat_history_failed', code });
        if (request.headers['forge-request-id'] !== id || request.headers['forge-envelope-version'] !== '1' ||
            request.headers['forge-contract-version'] !== '1') {
          fail(400, 'invalid_request');
          return;
        }
        if (destination === 'ide.mail.list' && body.kind === 'mail_list') {
          const items = statefulThreads.filter(t => (t.status === 'archived') === (body.archived === true)).map(t => ({ ...t, preview: body.include_metadata ? t.preview : undefined }));
          replyTo(200, { kind: 'mail_listed', items, next_cursor: null });
          return;
        }
        if (destination === 'ide.mail.open' && body.kind === 'mail_open') {
          const thread = statefulThreads.find(item => item.thread_id === body.thread_id);
          if (!thread) {
            replyTo(404, { kind: 'mail_failed', code: 'not_found' });
            return;
          }
          replyTo(200, { kind: 'mail_opened',
            thread: { ...thread, context_remaining_tokens: thread.status === 'archived' ? null : 1024, preview: body.include_metadata ? thread.preview : undefined },
            messages: [
              { sequence: 1, role: 'user', content: 'Review the fixture',
                ...(body.include_metadata === true ? { created_at: mailMessageDatesByThread[thread.thread_id][0] } : {}) },
              { sequence: 2, role: 'assistant', content: '## Fixture review\n\n' + mailPreview,
                ...(body.include_metadata === true ? { created_at: mailMessageDatesByThread[thread.thread_id][1] } : {}) },
            ],
            next_sequence: null,
          });
          return;
        }
        if (destination === 'ide.mail.archive' && body.kind === 'mail_archive_thread') {
          const thread = statefulThreads.find(item => item.thread_id === body.thread_id);
          if (!thread) { replyTo(404, { kind: 'mail_failed', code: 'not_found' }); return; }
          if (thread.status === 'archived') {
            if (thread.archive_reason === 'manual' && body.expected_revision === thread.revision - 1) {
              replyTo(200, { kind: 'mail_archived', thread_id: thread.thread_id, base_revision: thread.revision - 1, result_revision: thread.revision }); return;
            }
            replyTo(409, { kind: 'mail_failed', code: 'thread_archived' }); return;
          }
          if (thread.revision !== body.expected_revision) { replyTo(409, { kind: 'mail_failed', code: 'revision_conflict' }); return; }
          thread.status = 'archived';
          thread.archive_reason = 'manual';
          const base = thread.revision;
          thread.revision += 1;
          replyTo(200, { kind: 'mail_archived', thread_id: thread.thread_id, base_revision: base, result_revision: thread.revision });
          return;
        }
        if (destination === 'ide.chat.run.models' && body.kind === 'chat_run_models') {
          replyTo(200, { kind: 'chat_run_models_listed', default_model_id: 'gpt-oss-20b',
            models: [
              { model_id: 'gpt-oss-20b', label: 'GPT-OSS 20B (MXFP4)', ready: loadedModel === 'gpt-oss-20b' },
              { model_id: 'qwen2.5-coder-7b', label: 'Qwen2.5-Coder 7B Instruct (Q4_K_M)', ready: loadedModel === 'qwen2.5-coder-7b' },
            ] });
          return;
        }
        if (destination === 'ide.chat.run.submit' && body.kind === 'chat_run_submit') {
          const checkpoint = [...tabs.values()].find(tab =>
            tab.snapshot?.chat.pending_run?.idempotency_key === body.idempotency_key &&
            tab.snapshot.chat.pending_run.chat_id === body.chat_id &&
            tab.snapshot.chat.pending_run.model_id === body.model_id &&
            tab.snapshot.chat.conversations.some(chat => chat.chat_id === body.chat_id && chat.draft === body.content));
          if (!checkpoint) { fail(400, 'invalid_request'); return; }
          let run = submissions.get(body.idempotency_key);
          if (!run) {
            run = { run_id: randomUUID(), chat_id: body.chat_id, content: body.content, model_id: body.model_id,
              status: 'queued', events: [{ sequence: 1, kind: 'queued' }] };
            runs.set(run.run_id, run);
            submissions.set(body.idempotency_key, run);
            submitted++;
          } else if (run.chat_id !== body.chat_id || run.content !== body.content || run.model_id !== body.model_id) {
            fail(409, 'idempotency_conflict');
            return;
          }
          if (dropFirstAcknowledgement) {
            dropFirstAcknowledgement = false;
            fail(503, 'unavailable');
            return;
          }
          replyTo(202, { kind: 'chat_run_accepted', run_id: run.run_id, chat_id: run.chat_id, status: run.status });
          return;
        }
        if (destination === 'ide.chat.run.events' || destination === 'ide.chat.run.cancel') {
          const run = runs.get(body.run_id);
          if (!run || removed.has(run.chat_id)) { fail(404, 'not_found'); return; }
          const { run_id, chat_id } = run;
          if (destination === 'ide.chat.run.cancel' && body.kind === 'chat_run_cancel') {
            if (run.status === 'queued') {
              run.status = 'cancelled';
              run.events.push({ sequence: 2, kind: 'cancelled' });
            }
            replyTo(200, { kind: 'chat_run_cancelled', run_id, chat_id, status: run.status });
          } else if (body.kind === 'chat_run_events') {
            const events = run.events.filter(event => event.sequence > body.after_sequence);
            replyTo(200, { kind: 'chat_run_events', run_id, chat_id, status: run.status,
              worker_ready: false, events, next_sequence: events.at(-1)?.sequence ?? body.after_sequence,
              ...(run.status === 'completed' ? { answer: reply, revision: 1 } : {}) });
          } else fail(400, 'invalid_request');
          return;
        }
        if (destination === 'ide.chat.history.list' && body.kind === 'chat_history_list') {
          replyTo(200, { kind: 'chat_history_listed', items: [...history.values()].map(chat => chat.summary), next_cursor: null });
          return;
        }
        const chat = history.get(body.chat_id);
        if (!chat) { fail(404, 'not_found'); return; }
        if (destination === 'ide.chat.history.open' && body.kind === 'chat_history_open') {
          replyTo(200, { kind: 'chat_history_opened', chat });
          return;
        }
        if (body.expected_revision !== chat.summary.revision) { fail(409, 'revision_conflict'); return; }
        if (destination === 'ide.chat.history.retention.set' && body.kind === 'chat_history_retention_set') {
          const policy = body.policy;
          if (policy.kind === 'expires') {
            const end = new Date(`${policy.expires_on}T00:00:00Z`);
            if (!Number.isFinite(end.valueOf())) { fail(400, 'invalid_policy'); return; }
            end.setUTCDate(end.getUTCDate() + 1);
            chat.summary.retention = { ...policy, time_zone: 'UTC', expires_at: end.toISOString() };
          } else if (policy.kind === 'long_term') chat.summary.retention = { kind: 'long_term' };
          else { fail(400, 'invalid_policy'); return; }
          replyTo(200, { kind: 'chat_history_retention_set', chat_id: body.chat_id, revision: ++chat.summary.revision, retention: chat.summary.retention });
        } else if (destination === 'ide.chat.history.delete' && body.kind === 'chat_history_delete') {
          history.delete(body.chat_id);
          removed.add(body.chat_id);
          replyTo(200, { kind: 'chat_history_deleted', chat_id: body.chat_id, revision: chat.summary.revision + 1 });
        } else fail(400, 'invalid_request');
        return;
      }
      if (request.url !== '/v1/workspace' || !/^[0-9a-f]{32}$/.test(body.tab)) {
        send(404, {});
        return;
      }
      const previous = tabs.get(body.tab);
      if (body.operation === 'restore') {
        const lease = randomBytes(32).toString('hex');
        const preserve = body.preserve_chat === true;
        workspaceRestores.push({ preserve_chat: preserve, history: body.history === true, models: body.models === true });
        if (!(preserve || body.history === true) || preserve && ('history' in body || 'models' in body)) { send(400, {}); return; }
        const snapshot = previous?.snapshot ? structuredClone(previous.snapshot) : null;
        function savedConversation(chat, draft = '') {
          const { chat_id, revision } = chat.summary;
          return { chat_id, revision, draft, messages: chat.messages.map(({ role, content }) => ({ role, content })) };
        }
        const latest = preserve ? null : [...history.values()].at(-1);
        const restored = snapshot ?? (latest ? {
          chat: { selected: 0, conversations: [savedConversation(latest)] }, workbench: null, workbench_open: false,
        } : null);
        if (snapshot && !preserve) {
          const old = snapshot.chat;
          const kept = old.conversations.slice(0, old.selected).filter(item => !removed.has(item.chat_id)).length;
          old.conversations = old.conversations.filter(item => !removed.has(item.chat_id)).map(item =>
            history.has(item.chat_id) ? savedConversation(history.get(item.chat_id), item.draft) : item);
          old.selected = Math.min(kept, Math.max(0, old.conversations.length - 1));
          if (old.pending_run && removed.has(old.pending_run.chat_id)) delete old.pending_run;
        }
        tabs.set(body.tab, { lease, revision: 0, snapshot: restored });
        const visible = structuredClone(restored);
        if (visible && !preserve && body.models !== true) {
          delete visible.chat.model_id;
          if (visible.chat.pending_run) delete visible.chat.pending_run.model_id;
        }
        send(200, { lease, snapshot: visible });
      } else if (body.operation === 'save' && previous?.lease === body.lease) {
        if (body.revision > previous.revision) {
          previous.revision = body.revision;
          previous.snapshot = body.snapshot;

        }
        send(200, {});
      } else {
        send(401, {});
      }
    } catch {
      send(400, {});
    }
  });
  async function close() {
    try {
      if (server.listening) {
        const closed = once(server, 'close');
        server.close();
        server.closeAllConnections();
        await closed;
      }
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
  try {
    await writeFile(tokenFile, token, { mode: 0o600 });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    server.unref();
    return { endpoint: `127.0.0.1:${server.address().port}`, tokenFile,
      submitted: () => submitted, completions: () => completions, finish, close,
      workspaceRestores: () => structuredClone(workspaceRestores),
      seedWorkspace(tab, snapshot) {
        if (!/^[0-9a-f]{32}$/.test(tab) || !tabs.has(tab)) throw new Error('Seed requires an existing test-owned tab');
        tabs.get(tab).snapshot = structuredClone(snapshot);
      },
      workspaceSnapshot(tab) { return structuredClone(tabs.get(tab)?.snapshot ?? null); },
    };
  } catch (error) {
    await close();
    throw error;
  }
}
