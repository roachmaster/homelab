// Controlled localhost relay. Browser requests still use the generated Worker,
// real WASM and native host. The in-process store stands in for the runner here.
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const maxBytes = 2 * 1024 * 1024;

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
  let completions = 0;
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
      if (request.url === '/v1/chat/completions') {
        if (body.stream !== false || body.messages?.length !== 1 ||
            body.messages[0].role !== 'user' || body.messages[0].content !== prompt ||
            !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(body.chat_id)) {
          send(400, {});
          return;
        }
        completions += 1;
        const message = { role: 'assistant', content: reply };
        const revision = 1;
        history.set(body.chat_id, {
          summary: {
            chat_id: body.chat_id, title: prompt, updated_at: '2026-11-16T12:00:00Z', revision,
            retention: { kind: 'expires', expires_on: '2026-12-16', time_zone: 'UTC', expires_at: '2026-12-17T00:00:00Z' },
          },
          messages: [...body.messages, message].map((entry, index) => ({ ...entry, position: index + 1 })),
        });
        send(200, { chat_id: body.chat_id, revision, choices: [{ message }] });
        return;
      }
      if (request.url === '/v1/dispatch') {
        const id = body.request_id;
        const replyTo = (status, event) => send(status, { protocol_version: 1, event: { request_id: id, ...event } });
        const fail = (status, code) => replyTo(status, { kind: 'chat_history_failed', code });
        if (request.headers['forge-request-id'] !== id || request.headers['forge-envelope-version'] !== '1' ||
            request.headers['forge-contract-version'] !== '1') {
          fail(400, 'invalid_request');
          return;
        }
        const destination = request.headers['forge-destination'];
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
        if (body.history !== true) { send(400, {}); return; }
        const snapshot = previous?.snapshot ? structuredClone(previous.snapshot) : null;
        function savedConversation(chat, draft = '') {
          const { chat_id, revision } = chat.summary;
          return { chat_id, revision, draft, messages: chat.messages.map(({ role, content }) => ({ role, content })) };
        }
        const latest = [...history.values()].at(-1);
        const restored = snapshot ?? (latest ? {
          chat: { selected: 0, conversations: [savedConversation(latest)] }, workbench: null, workbench_open: false,
        } : null);
        if (snapshot) {
          const old = snapshot.chat;
          const kept = old.conversations.slice(0, old.selected).filter(item => !removed.has(item.chat_id)).length;
          old.conversations = old.conversations.filter(item => !removed.has(item.chat_id)).map(item =>
            history.has(item.chat_id) ? savedConversation(history.get(item.chat_id), item.draft) : item);
          old.selected = Math.min(kept, Math.max(0, old.conversations.length - 1));
        }
        tabs.set(body.tab, { lease, revision: 0, snapshot: restored });
        send(200, { lease, snapshot: restored });
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
    return { endpoint: `127.0.0.1:${server.address().port}`, tokenFile, completions: () => completions, close };
  } catch (error) {
    await close();
    throw error;
  }
}
