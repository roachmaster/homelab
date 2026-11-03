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
const status = { id: 'collection-status', textContent: 'Starting', dataset: {} };
// Existing row fixture rejects unnecessary moves. Real selection/filter/page
// changes remain covered by the actual-browser suite, not this minimal DOM.
const attributes = () => ({ setAttribute() {}, removeAttribute() {} });
const select = { ...attributes(), children: [{}, {}, {}], getAttribute() { return 'false'; }, tabIndex: 0 };
const toggle = attributes();
const item = { dataset: { rowId: 'record-1' }, children: [toggle, select] };
const tree = {
  id: 'collection-items', children: [item], scrollTop: 144,
  querySelectorAll() { return [select]; },
  append() { throw new Error('Unchanged list row was reordered'); },
  insertBefore() { throw new Error('Unchanged list row was moved'); },
};
const body = { isConnected: true };
const document = {
  body, activeElement: body,
  querySelectorAll(selector) { if (selector === '[data-bind-text]') return [status]; return selector === 'input' || selector === 'input[data-action]' ? [input] : []; },
  querySelector() { return null; },
  getElementById(id) { return id === input.id ? input : id === status.id ? status : id === tree.id ? tree : null; },
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
// ABI2 views without optional additive tone metadata remain displayable. Unknown
// values cannot inject presentation selectors, and lifecycle erases old urgency.
function textView(tone) {
  workers.at(-1).onmessage({ data: { kind: 'view', view: {
    texts: [{ id: status.id, text: 'Bounded notice', ...(tone === undefined ? {} : { tone }) }],
  } } });
}
textView('warning'); assert.equal(status.dataset.tone, 'warning');
textView(); assert.equal(status.dataset.tone, 'neutral');
textView('error'); assert.equal(status.dataset.tone, 'error');
textView('warning injected'); assert.equal(status.dataset.tone, 'neutral');
textView('accent'); assert.equal(status.dataset.tone, 'accent');
workers.at(-1).onmessage({ data: { kind: 'view', view: { lists: [{
  id: tree.id, rows: [{ id: 'record-1', label: 'Owned fixture', glyph: 'R',
    kind_label: 'record', parent: null, secondary: '', depth: 0, selected: false,
    expanded: null, action: { kind: 'choose_record' }, toggle: null }],
}] } } });
assert.equal(tree.children[0], item);
assert.equal(tree.scrollTop, 144, 'same-order list projections preserve tree scroll');
// Generic pane context is opaque: external changes reset stale pane focus and
// scroll, unchanged context and direct in-pane navigation retain ownership.
const paneBody = { scrollTop: 900 };
const inside = { isConnected: true, closest() { return null; } };
const pane = { id: 'collection-pane', dataset: { pane: 'main' }, hidden: false,
  contains(node) { return node === inside; },
  querySelector(selector) { return selector === '.fg-pane-body' ? paneBody : null; },
};
const paneState = { root: { dataset: {} }, grid: { clientWidth: 0 }, panes: [pane], main: pane,
  open: new Set([pane.id]), sizes: new Map(), active: pane.id, narrow: { matches: false },
  focus: new Map([[pane.id, { node: inside }]]), context: new Map(), resetScroll: new Set(),
};
context.paneState = paneState;
vm.runInContext("layouts.set('collection-layout', paneState)", context);
function paneView(contextValue) {
  workers.at(-1).onmessage({ data: { kind: 'view', view: {
    panes: [{ id: pane.id, context: contextValue }],
  } } });
}
paneView({ selected: 1 });
assert.equal(paneBody.scrollTop, 0, 'external selection reveals new pane content');
assert.equal(paneState.focus.size, 0, 'external selection discards stale focus');
paneBody.scrollTop = 500; paneState.focus.set(pane.id, { node: inside });
paneView({ selected: 1 });
assert.equal(paneBody.scrollTop, 500, 'same context preserves scroll');
assert.equal(paneState.focus.size, 1, 'same context preserves saved focus');
document.activeElement = inside;
paneView({ selected: 2 });
assert.equal(paneBody.scrollTop, 500, 'direct in-pane navigation preserves scroll');
assert.equal(paneState.focus.size, 1, 'direct in-pane navigation preserves focus');
document.activeElement = body;
paneView({ selected: 3 });
assert.equal(paneBody.scrollTop, 0);
assert.equal(paneState.focus.size, 0);
// Hidden panes have no scroll box: retain the reset until activation shows it.
paneState.narrow.matches = true; paneState.active = 'other-pane';
paneBody.scrollTop = 700; paneState.focus.set(pane.id, { node: inside });
paneView({ selected: 4 });
assert.equal(pane.hidden, true);
assert.equal(paneBody.scrollTop, 700, 'hidden pane reset is deferred');
assert.equal(paneState.resetScroll.has(pane.id), true);
assert.equal(paneState.focus.size, 0);
paneState.active = pane.id;
vm.runInContext('updateLayout(paneState)', context);
assert.equal(pane.hidden, false);
assert.equal(paneBody.scrollTop, 0, 'activation consumes pending reset after showing pane');
assert.equal(paneState.resetScroll.size, 0);
paneBody.scrollTop = 350;
vm.runInContext('updateLayout(paneState)', context);
assert.equal(paneBody.scrollTop, 350, 'consumed reset does not keep snapping scroll');
paneState.resetScroll.add(pane.id);
vm.runInContext('clearProtected()', context);
assert.equal(paneState.resetScroll.size, 0, 'protected lifecycle clears pending resets');
assert.equal(paneState.context.size, 0);
vm.runInContext("layouts.delete('collection-layout')", context);
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
assert.equal(input.value, ''); assert.equal(status.dataset.tone, undefined);
assert.equal(status.textContent, 'Starting'); lifecycle.get('pageshow')();
view('', null, 'C'); type('new');
assert.equal(workers[1].sent[0].presentation_sequence, 1);
assert.equal(workers[1].sent[0].event.report.token, 'C');
console.log('Input presentation sequencing: repeated values, backspace, rejection, invalid draft, context and lifecycle passed.');

