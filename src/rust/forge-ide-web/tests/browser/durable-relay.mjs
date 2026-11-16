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
  let history = [];
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
            body.messages[0].role !== 'user' || body.messages[0].content !== prompt) {
          send(400, {});
          return;
        }
        completions += 1;
        send(200, { choices: [{ message: { role: 'assistant', content: reply } }] });
        return;
      }
      if (request.url !== '/v1/workspace' || !/^[0-9a-f]{32}$/.test(body.tab)) {
        send(404, {});
        return;
      }
      const previous = tabs.get(body.tab);
      if (body.operation === 'restore') {
        const lease = randomBytes(32).toString('hex');
        const snapshot = previous?.snapshot ?? (history.length === 0 ? null : {
          chat: { selected: 0, conversations: history }, workbench: null, workbench_open: false,
        });
        tabs.set(body.tab, { lease, revision: 0, snapshot });
        send(200, { lease, snapshot });
      } else if (body.operation === 'save' && previous?.lease === body.lease) {
        if (body.revision > previous.revision) {
          previous.revision = body.revision;
          previous.snapshot = body.snapshot;
          history = body.snapshot.chat.conversations.filter(({ messages }) => messages.length)
            .map(({ messages }) => ({ messages, draft: '' }));
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
