// Fault-injection harness for GENERATED worker glue, never a browser asset.
// It runs the actual WASM model in Node's vm, with simulated fetch responses.
// This does not replace real browser -> native host -> agent acceptance tests.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import vm from 'node:vm';
const assetDir = process.argv[2];
assert.ok(assetDir, 'supply a generated asset directory containing worker.js/app.wasm');
const source = await readFile(join(assetDir, 'worker.js'), 'utf8');
const wasmBytes = await readFile(join(assetDir, 'app.wasm'));
const copy = value => JSON.parse(JSON.stringify(value));
const json = (value, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'Content-Type': 'application/json' } });
const catalog = { protocol_version: 1, session_epoch: 'test-session', timeout_ms: 1000, nodes: [{ key: 'lab', label: 'Lab', node: 'node', incarnation: 'boot', projects: [{ alias: 'project', label: 'Project' }] }] };
const paired = request => ({ protocol_version: 1, event: { kind: 'paired', request_id: request.request_id, catalog }, csrf: 'a'.repeat(64) });
const report = { format_version: 1, complete: true, files: 0, bytes_read: 0, entries: [{ id: 0, parent: null, name: { encoding: 'utf8', value: '<script>text, not HTML</script>' }, kind: 'directory', size_bytes: null, lines: null, content_status: '', diagnostics: [], declarations: [] }] };
const inspected = request => ({ protocol_version: 1, event: { kind: 'result', result: { protocol_version: 1, token: request.request.token, node_key: request.request.node_key, project: request.request.project, outcome: { kind: 'report', report } } } });
async function until(predicate, label) {
  const end = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > end) throw new Error(`Timed out: ${label}`);
    await new Promise(resolve => setTimeout(resolve, 2));
  }
}
async function harness(respond, initialMode = 'normal') {
  const messages = [], requests = [], timers = new Set();
  let mode = initialMode;
  const context = vm.createContext({
    TextEncoder, TextDecoder, Uint8Array, AbortController, Response, TypeError,
    Date: class extends Date { static now() { return 1000; } },
    WebAssembly: {
      Memory: WebAssembly.Memory,
      instantiate: async (...args) => {
        const compiled = await WebAssembly.instantiate(...args);
        const actual = compiled.instance.exports;
        return { instance: { exports: { ...actual,
          abi_version: () => mode === 'legacy' ? 1 : actual.abi_version(),
          dispatch_input: () => mode === 'zero' ? 0 : actual.dispatch_input(),
          output_len: () => mode === 'malformed' ? 1 : actual.output_len(),
        } } };
      },
    },
    setTimeout: (f, n) => { const id = setTimeout(() => { timers.delete(id); f(); }, n); timers.add(id); return id; },
    clearTimeout: id => { timers.delete(id); clearTimeout(id); },
    postMessage: message => messages.push(copy(message)),
    fetch: async (url, options = {}) => {
      if (url === '/app.wasm') return new Response(wasmBytes);
      assert.equal(url, '/forge/v1/dispatch');
      assert.equal(options.method, 'POST');
      assert.equal(options.credentials, 'same-origin');
      assert.equal(options.redirect, 'error');
      const body = JSON.parse(options.body);
      requests.push({ body, headers: options.headers, signal: options.signal });
      return respond(body, options);
    },
  });
  vm.runInContext(source, context, { filename: 'generated-worker.js', timeout: 3000 });
  const status = () => messages.filter(m => m.kind === 'view').at(-1)?.view.texts.find(t => t.id === 'status')?.text ?? '';
  return {
    messages, requests, status,
    send: event => context.onmessage({ data: event }),
    corrupt: value => { mode = value; },
    close: () => { for (const id of timers) clearTimeout(id); timers.clear(); },
  };
}
for (const [statusCode, expected] of [[401, /Access denied/], [403, /Access denied/], [413, /Limit exceeded/], [503, /Unavailable/]]) {
  const h = await harness(() => json({ error: 'rejected' }, statusCode));
  try {
    await until(() => expected.test(h.status()), `HTTP ${statusCode} -> typed failure`);
    assert.equal(h.requests.length, 1);
    assert.equal(h.messages.some(m => m.kind === 'fatal'), false);
  } finally { h.close(); }
}
{
  const h = await harness(request => request.kind === 'resume' ? json({}, 401) : request.kind === 'pair' ? json(paired(request)) : json(inspected(request)));
  try {
    await until(() => /Access denied/.test(h.status()), 'unpaired resume');
    await h.send({ kind: 'pair', value: 'b'.repeat(64) });
    await until(() => /Connected/.test(h.status()), 'actual WASM pairing');
    await h.send({ kind: 'inspect' });
    await until(() => /Inspection ready/.test(h.status()), 'actual WASM inspection');
    const request = h.requests.find(r => r.body.kind === 'inspect');
    assert.equal(request.headers['Forge-Destination'], 'ide.inspect');
    assert.equal(request.headers['Forge-Target-Node'], 'node');
    assert.equal(request.headers['Forge-CSRF'], 'a'.repeat(64));
    assert.equal(request.body.request.project, 'project');
    assert.equal(h.messages.filter(m => m.kind === 'view').at(-1).view.lists[0].rows[0].label, '<script>text, not HTML</script>');
  } finally { h.close(); }
}
{
  let complete;
  const h = await harness(request => request.kind === 'resume' ? json(paired(request)) : new Promise(resolve => { complete = () => resolve(json(inspected(request))); }));
  try {
    await until(() => /Connected/.test(h.status()), 'resume success');
    await h.send({ kind: 'inspect' });
    await until(() => !!complete, 'in-flight simulated fetch');
    await h.send({ kind: 'cancel' });
    assert.match(h.status(), /Stopped waiting locally/);
    assert.equal(h.requests.find(r => r.body.kind === 'inspect').signal.aborted, true);
    complete();
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.match(h.status(), /Stopped waiting locally/);
    assert.equal(h.messages.filter(m => m.kind === 'view').at(-1).view.lists[0].rows.length, 0);
  } finally { h.close(); }
}
for (const mode of ['zero', 'malformed']) {
  const h = await harness(request => json(paired(request)));
  try {
    await until(() => /Connected/.test(h.status()), 'startup before ABI failure');
    h.corrupt(mode);
    await h.send({ kind: 'inspect' });
    await until(() => h.messages.some(m => m.kind === 'fatal'), `ABI ${mode} fail closed`);
    const count = h.requests.length;
    const messages = h.messages.length;
    await h.send({ kind: 'inspect' });
    assert.equal(h.requests.length, count, 'closed worker must not send another request');
    assert.equal(h.messages.length, messages, 'closed worker ignores later events');
  } finally { h.close(); }
}
console.log('Generated-worker fault harness passed: HTTP classification, actual WASM pair/inspect/cancel/stale state, and ABI fail-closed lifecycle. Fetch is simulated; this is not browser/network proof.');

{
  const h = await harness(request => json(paired(request)), 'legacy');
  try {
    await until(() => h.messages.some(m => m.kind === 'fatal'), 'legacy ABI rejects before session resume');
    assert.equal(h.requests.length, 0, 'ABI mismatch must not invoke a backend effect');
    const messages = h.messages.length;
    await h.send({ kind: 'inspect' });
    assert.equal(h.requests.length, 0);
    assert.equal(h.messages.length, messages);
  } finally { h.close(); }
}
console.log('Generated worker rejects legacy ABI 1 before session or inspection effects.');

{
  const h = await harness(request => request.kind === 'resume' ? json(paired(request)) : json(inspected(request)));
  try {
    await until(() => /Connected/.test(h.status()), 'resume before presentation envelope');
    await h.send({ event: { kind: 'inspect' }, presentation_sequence: 5 });
    await until(() => /Inspection ready/.test(h.status()), 'enveloped event reaches actual WASM');
    assert.ok(h.messages.some(m => m.kind === 'view' && m.presentation_sequence === 5));
    assert.equal(h.messages.filter(m => m.kind === 'view').at(-1).presentation_sequence, null,
      'asynchronous effect result must not acknowledge an input draft');
    const reportKey = h.requests.find(r => r.body.kind === 'inspect').body.request.token;
    const before = h.requests.length;
    await h.send({ event: { kind: 'set_file_filter', report: reportKey, value: 'text' }, presentation_sequence: 6 });
    const accepted = h.messages.filter(m => m.kind === 'view').at(-1);
    assert.equal(accepted.presentation_sequence, 6);
    assert.equal(accepted.view.inputs.find(i => i.id === 'file-filter').value, 'text');
    assert.equal(h.requests.length, before, 'local filter adds no transport effect');
    await h.send({ event: { kind: 'inspect' }, presentation_sequence: 0 });
    await until(() => h.messages.some(m => m.kind === 'fatal'), 'invalid presentation sequence fails closed');
    await h.send({ kind: 'inspect' });
    assert.equal(h.requests.length, before, 'invalid envelope cannot start or resume effects');
  } finally { h.close(); }
}
console.log('Generated worker strips bounded presentation envelopes before actual WASM; only the matching synchronous view acknowledges them.');
