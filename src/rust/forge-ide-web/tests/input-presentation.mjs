// Test-only deterministic DOM/worker boundary fixture. Runs the actual generated
// MAIN emitter, not a copied reducer. This is not an application runtime asset.
// Real DOM, real WASM and admission acceptance remain in tests/browser/journey.mjs.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

if (process.argv.length !== 3) throw new Error('usage: node input-presentation.mjs GENERATED_MAIN_JS');
const source = await readFile(process.argv[2], 'utf8');
const handlers = new Map();
const lifecycle = new Map();
const workers = [];
const attrs = new Map([['data-action', 'filter_collection'], ['data-dynamic-action', '']]);
const input = {
  id: 'collection-filter', value: '', disabled: true, isConnected: true,
  dataset: { action: 'filter_collection' }, validity: '',
  closest() { return null; },
  hasAttribute(name) { return attrs.has(name); },
  setAttribute(name, value) { attrs.set(name, value); },
  removeAttribute(name) { attrs.delete(name); },
  addEventListener(name, fn) { handlers.set(name, fn); },
  setCustomValidity(value) { this.validity = value; },
  reportValidity() { return !this.validity; },
};
const body = { isConnected: true };
const document = {
  body, activeElement: body,
  querySelectorAll(selector) { return selector === 'input' || selector === 'input[data-action]' ? [input] : []; },
  querySelector() { return null; },
  getElementById(id) { return id === input.id ? input : null; },
};
const context = vm.createContext({
  document, TextEncoder,
  window: { addEventListener(name, fn) { lifecycle.set(name, fn); } },
  Worker: class {
    constructor() { this.sent = []; this.stopped = false; workers.push(this); }
    postMessage(event) { this.sent.push(event); }
    terminate() { this.stopped = true; }
  },
});
vm.runInContext(source, context, { filename: process.argv[2] });
const action = token => ({ kind: 'filter_collection', report: { token } });
function view(value, sequence, token = 'A', disabled = false) {
  workers.at(-1).onmessage({ data: { kind: 'view', presentation_sequence: sequence,
    view: { inputs: [{ id: input.id, value, disabled, max_bytes: 256, action: action(token) }] },
  } });
}
function type(value) { input.value = value; handlers.get('input')(); }
view('', null);
// Repeated values must not falsely acknowledge the newest edit.
type('a'); type('ab'); type('a');
assert.deepEqual(workers[0].sent.map(m => m.presentation_sequence), [1, 2, 3]);
assert.deepEqual(workers[0].sent.map(m => m.event.value), ['a', 'ab', 'a']);
assert.equal(workers[0].sent[0].event.report.token, 'A');
view('a', 1); assert.equal(input.value, 'a');
view('ab', 2); assert.equal(input.value, 'a', 'older echo must not discard a newer backspace');
view('', null); assert.equal(input.value, 'a', 'unrelated view must not replace pending draft');
view('a', 3); assert.equal(input.value, 'a');
// A latest acknowledgement reconciles even a rejected input to canonical WASM state.
type('rejected'); view('a', 4); assert.equal(input.value, 'a');
// Clear/backspace is a real edit, not a missing value.
type('ab'); type(''); view('ab', 5); assert.equal(input.value, ''); view('', 6);
// Oversized drafts are explicitly invalid and unsent, including while old acks arrive.
type('legal'); const sent = workers[0].sent.length;
type('é'.repeat(129)); assert.equal(workers[0].sent.length, sent);
assert.equal(attrs.get('aria-invalid'), 'true');
assert.match(input.validity, /has not been applied/);
view('legal', 7); assert.equal(input.value, 'é'.repeat(129));
type('fixed'); view('fixed', 8); assert.equal(input.value, 'fixed');
assert.equal(attrs.has('aria-invalid'), false);
// Changed action context and disabled views always clear drafts and validity.
type('pending'); view('', null, 'B'); assert.equal(input.value, '');
type('é'.repeat(129)); view('', null, 'B', true); assert.equal(input.value, '');
assert.equal(input.validity, ''); assert.equal(attrs.has('aria-invalid'), false);
// Teardown erases drafts and counter; a fresh worker does not inherit them.
lifecycle.get('pagehide')(); assert.equal(workers[0].stopped, true);
assert.equal(input.value, ''); lifecycle.get('pageshow')();
view('', null, 'C'); type('new');
assert.equal(workers[1].sent[0].presentation_sequence, 1);
assert.equal(workers[1].sent[0].event.report.token, 'C');
console.log('Input presentation sequencing: repeated values, backspace, rejection, invalid draft, context and lifecycle passed.');
