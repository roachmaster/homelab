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
const catalog = { protocol_version: 1, session_epoch: 'test-session', timeout_ms: 1000, nodes: [{ key: 'lab', label: 'Lab', node: 'node', incarnation: 'boot', projects: [{ alias: 'project', label: 'Project' }, { alias: 'other', label: 'Other' }] }] };
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
    await until(() => /Project ready/.test(h.status()), 'actual WASM inspection');
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
    await until(() => /Project ready/.test(h.status()), 'enveloped event reaches actual WASM');
    assert.ok(h.messages.some(m => m.kind === 'view' && m.presentation_sequence === 5));
    assert.equal(h.messages.filter(m => m.kind === 'view').at(-1).presentation_sequence, null,
      'asynchronous effect result must not acknowledge an input draft');
    const reportKey = h.requests.find(r => r.body.kind === 'inspect').body.request.token;
    const before = h.requests.length;
    const current = () => h.messages.filter(m => m.kind === 'view').at(-1);
    const query = () => current().view.inputs.find(i => i.id === 'project-query');
    await h.send({ event: { kind: 'set_explorer_query', report: reportKey, entry_names: false, value: 'text' }, presentation_sequence: 6 });
    const accepted = current();
    assert.equal(accepted.presentation_sequence, 6);
    assert.equal(query().value, 'text');
    assert.equal(query().action.kind, 'set_explorer_query');
    assert.equal(query().action.entry_names, false);
    assert.deepEqual(query().action.report, reportKey);
    assert.equal(accepted.view.inputs.some(i => i.id === 'file-filter'), false, 'the unified input replaces the separate entry filter');
    await h.send({ event: { kind: 'toggle_explorer_filter', report: reportKey }, presentation_sequence: 7 });
    assert.equal(current().presentation_sequence, 7);
    assert.equal(query().value, 'text', 'switching to entry filtering preserves the query');
    assert.equal(query().action.entry_names, true);
    assert.equal(current().view.buttons.find(button => button.id === 'explorer-filter-mode').pressed, true);
    const entryView = copy(current().view);
    await h.send({ event: { kind: 'set_explorer_query', report: reportKey, entry_names: false, value: 'late edit' }, presentation_sequence: 8 });
    assert.equal(current().presentation_sequence, 8);
    assert.deepEqual(current().view, entryView, 'an old search-mode edit cannot alter the current filter');
    await h.send({ event: { kind: 'set_explorer_query', report: reportKey, entry_names: true, value: 'script' }, presentation_sequence: 9 });
    assert.equal(current().presentation_sequence, 9);
    assert.equal(query().value, 'script');
    assert.equal(h.requests.length, before, 'query edits and mode changes add no transport effect');

    await h.send({ event: { kind: 'inspect' }, presentation_sequence: 10 });
    await until(() => JSON.stringify(query()?.action?.report) !== JSON.stringify(reportKey)
      && /Project ready/.test(h.status()), 'refresh accepts a new report context');
    const refreshed = copy(current().view);
    const afterRefresh = h.requests.length;
    assert.equal(afterRefresh, before + 1, 'explicit refresh sends one inspection request');
    await h.send({ event: { kind: 'set_explorer_query', report: reportKey, entry_names: query().action.entry_names, value: 'stale report edit' }, presentation_sequence: 11 });
    assert.equal(current().presentation_sequence, 11);
    assert.deepEqual(current().view, refreshed, 'an old report edit cannot alter the refreshed view');
    assert.equal(h.requests.length, afterRefresh, 'stale filter events cannot start transport effects');
    await h.send({ event: { kind: 'inspect' }, presentation_sequence: 0 });
    await until(() => h.messages.some(m => m.kind === 'fatal'), 'invalid presentation sequence fails closed');
    await h.send({ kind: 'inspect' });
    assert.equal(h.requests.length, afterRefresh, 'invalid envelope cannot start or resume effects');
  } finally { h.close(); }
}
console.log('Generated worker strips bounded presentation envelopes before actual WASM; only the matching synchronous view acknowledges them.');

{
  const single = copy(catalog);
  single.nodes[0].projects.length = 1;
  const withSource = copy(report);
  withSource.files = 1;
  withSource.bytes_read = 12;
  withSource.entries.push({ id: 1, parent: 0, name: { encoding: 'utf8', value: 'lib.rs' }, kind: 'file', size_bytes: 12, lines: 1, content_status: 'text', diagnostics: [], declarations: [] });
  let deny = true;
  const h = await harness(request => {
    if (request.kind === 'resume') {
      const reply = paired(request); reply.event.catalog = single; return json(reply);
    }
    if (request.kind === 'inspect') {
      const reply = inspected(request); reply.event.result.outcome.report = withSource; return json(reply);
    }
    assert.equal(request.kind, 'source');
    if (deny) return json({ error: 'denied' }, 401);
    const r = request.request;
    return json({ protocol_version: 1, event: { kind: 'source_result', result: { protocol_version: 1, token: r.token, report: r.report, node_key: r.node_key, project: r.project, entry: r.entry, outcome: { kind: 'source', document: { text: 'fn run() {}', sha256: 'a'.repeat(64) } } } } });
  });
  try {
    await until(() => /Project ready/.test(h.status()), 'single project opens automatically');
    const key = h.requests.find(r => r.body.kind === 'inspect').body.request.token;
    const current = () => h.messages.filter(m => m.kind === 'view').at(-1).view;
    await h.send({ kind: 'select_entry', report: key, value: 1 });
    await until(() => /Access denied/.test(current().texts.find(t => t.id === 'source-status').text), 'source HTTP failure is associated with its source request');
    assert.match(h.status(), /Project ready/, 'source failure retains the accepted inspection');
    assert.equal(current().sources[0].text, '');
    deny = false;
    await h.send({ kind: 'select_entry', report: key, value: 1 });
    await until(() => current().sources[0].text === 'fn run() {}', 'explicit source retry displays accepted bytes');
    const source = h.requests.find(r => r.body.kind === 'source');
    assert.equal(source.headers['Forge-Destination'], 'ide.source');
    assert.equal(source.headers['Forge-CSRF'], 'a'.repeat(64));
    assert.deepEqual(source.body.request.report, key);
    assert.equal(h.messages.some(m => m.kind === 'fatal'), false);
  } finally { h.close(); }
}
console.log('Generated worker auto-opens one project and sends guarded source effects; source denial retains the inspection and explicit retry succeeds.');

{
  // Pairing in a single-project installation starts inspection and model
  // discovery together. Previous chats must still open on the first click.
  const single = copy(catalog);
  single.nodes[0].projects.length = 1;
  single.chat_history = true;
  single.async_chat = true;
  const savedChat = {
    chat_id: '8452b258-df30-4985-bfa1-cd98245b2a06',
    title: 'Saved fixture chat',
    updated_at: '2026-11-16T12:00:00Z',
    revision: 1,
    retention: { kind: 'long_term' },
  };
  let releaseInspection, releaseModels;
  const h = await harness(request => {
    const reply = event => json({ protocol_version: 1, event: { request_id: request.request_id, ...event } });
    switch (request.kind) {
      case 'resume': {
        const response = paired(request);
        response.event.catalog = single;
        return json(response);
      }
      case 'inspect':
        return new Promise(resolve => { releaseInspection = () => resolve(json(inspected(request))); });
      case 'chat_run_models':
        return new Promise(resolve => { releaseModels = () => resolve(reply({
          kind: 'chat_run_models_listed', default_model_id: 'gpt-oss-20b',
          models: [{ model_id: 'gpt-oss-20b', label: 'GPT-OSS 20B', ready: true }],
        })); });
      case 'chat_history_list':
        return reply({ kind: 'chat_history_listed', items: [savedChat], next_cursor: null });
      default:
        throw new Error('Unexpected transport request in startup overlap fixture');
    }
  });
  try {
    const historyFetches = () => h.requests.filter(request => request.body.kind === 'chat_history_list');
    const historyStatus = () => h.messages.filter(message => message.kind === 'view').at(-1)
      ?.view.texts.find(text => text.id === 'chat-history-status')?.text ?? '';
    await until(() => releaseInspection && releaseModels, 'both startup transport slots occupied');
    await h.send({ kind: 'show_chat_history' });
    assert.equal(historyFetches().length, 0, 'history waits without exceeding local concurrency');
    assert.equal(historyStatus(), 'Checking saved chats…');
    releaseModels();
    await until(() => h.messages.some(message => message.kind === 'view'
      && message.view.lists.some(list => list.id === 'chat-history-list'
        && list.rows.some(row => row.id === savedChat.chat_id))), 'first history click displays saved chats');
    assert.equal(historyFetches().length, 1, 'single history request runs after a slot opens');
    assert.equal(historyFetches()[0].headers['Forge-Destination'], 'ide.chat.history.list');
    releaseInspection();
    await until(() => /Project ready/.test(h.status()), 'original inspection completes independently');
    assert.equal(h.messages.some(message => message.kind === 'fatal'), false);
  } finally { h.close(); }
}
console.log('Generated worker waits for a transport slot to open Previous chats during overlapping startup effects.');
