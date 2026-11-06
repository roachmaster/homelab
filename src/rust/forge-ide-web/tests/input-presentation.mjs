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
const extraElements = new Map();
const sourceElements = [];
function sourceNode(tag = 'span') {
  return { tagName: tag.toUpperCase(), dataset: {}, children: [], textContent: '',
    isConnected: true, scrollLeft: 0, scrollTop: 0, clientTop: 1, scrolled: false,
    closest(selector) { return selector === '[data-source-view]' && this.hasAttribute(selector.slice(1, -1)) ? this : this.parentElement?.closest?.(selector) ?? null; },
    getBoundingClientRect() {
      if (this.tagName === 'PRE') return { top: 100 };
      if (this.className === 'fg-source-line') {
        const source = this.parentElement;
        return { top: 100 + source.clientTop + 6 + source.children.indexOf(this) * 20 - source.scrollTop };
      }
      return { top: 0 };
    },
    setAttribute(name, value) { this[name] = value; },
    hasAttribute(name) { return name === 'data-source-view' && this.tagName === 'PRE'; },
    append(...children) { for (const child of children) child.parentElement = this; this.children.push(...children); },
    replaceChildren(...children) { for (const child of children) child.parentElement = this; this.children = children; },
    scrollIntoView() { this.scrolled = true; },
    focus() { document.activeElement = this; },
  };
}
const document = {
  body, activeElement: body,
  createElement(tag) { return sourceNode(tag); },
  querySelectorAll(selector) { if (selector.includes('[data-source-view]')) return sourceElements; if (selector === '[data-bind-text]') return [status]; return selector === 'input' || selector === 'input[data-action]' ? [input] : []; },
  querySelector() { return null; },
  getElementById(id) { return id === input.id ? input : id === status.id ? status : id === tree.id ? tree : extraElements.get(id) ?? null; },
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
// ABI3 views without optional additive tone metadata remain displayable. Unknown
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
const inside = { isConnected: true, disabled: false, closest(selector) { return selector === '[data-pane]' ? pane : null; }, hasAttribute() { return false; }, dataset: {} };
const outside = { isConnected: true, disabled: false, closest(selector) { return selector === '[data-pane]' ? outsidePane : null; }, hasAttribute() { return false; }, dataset: {} };
const outsidePane = {};
const heading = { isConnected: true, closest() { return null; }, focus() { document.activeElement = heading; } };
extraElements.set('collection-pane-heading', heading);
let navigated = null;
const pane = { id: 'collection-pane', dataset: { pane: 'main' }, hidden: false,
  contains(node) { return node === inside || node === heading || node === navigated; },
  querySelector(selector) { return selector === '.fg-pane-body' ? paneBody : null; },
};
const paneState = { root: { dataset: {} }, grid: { clientWidth: 0 }, panes: [pane], main: pane,
  open: new Set([pane.id]), sizes: new Map(), active: pane.id, narrow: { matches: false },
  focus: new Map([[pane.id, { node: inside }]]), context: new Map(), reveal: new Map(), resetScroll: new Set(),
};
context.paneState = paneState;
vm.runInContext("layouts.set('collection-layout', paneState)", context);
function paneView(contextValue, sequence = null, reveal = null) {
  workers.at(-1).onmessage({ data: { kind: 'view', presentation_sequence: sequence, view: {
    panes: [{ id: pane.id, context: contextValue, reveal }],
  } } });
}
paneView({ selected: 1 });
assert.equal(paneBody.scrollTop, 0, 'external selection reveals new pane content');
assert.equal(paneState.focus.size, 0, 'external selection discards stale focus');
paneBody.scrollTop = 500; paneState.focus.set(pane.id, { node: inside });
paneView({ selected: 1 });
assert.equal(paneBody.scrollTop, 500, 'same context preserves scroll');
assert.equal(paneState.focus.size, 1, 'same context preserves saved focus');
context.inside = inside; context.outside = outside;
vm.runInContext("actionByElement.set(inside,{kind:'choose'});actionByElement.set(outside,{kind:'choose'});", context);
function dispatchFrom(node) {
  vm.runInContext(`sendAction(${node})`, context);
  return workers.at(-1).sent.at(-1).presentation_sequence;
}
document.activeElement = inside;
const directSequence = dispatchFrom('inside');
paneView({ selected: 2 }, directSequence);
assert.equal(paneBody.scrollTop, 500, 'direct in-pane navigation preserves scroll');
assert.equal(paneState.focus.size, 1, 'direct in-pane navigation preserves focus');
const externalSequence = dispatchFrom('outside');
// The user switches back before the external action's accepted view arrives.
document.activeElement = inside;
paneView({ selected: 3 }, externalSequence);
assert.equal(document.activeElement, heading, 'external context repairs only stale focus inside its pane');
assert.equal(paneBody.scrollTop, 0);
assert.equal(paneState.focus.size, 0);
// An older acknowledgement cannot override newer direct user intent.
paneBody.scrollTop = 500; document.activeElement = inside;
const olderExternal = dispatchFrom('outside');
const newerDirect = dispatchFrom('inside');
paneView({ selected: 30 }, olderExternal);
assert.equal(paneBody.scrollTop, 500);
assert.equal(document.activeElement, inside);
paneView({ selected: 31 }, newerDirect);
assert.equal(paneBody.scrollTop, 500);
// Switching away before a valid direct acknowledgement does not discard that
// pane's saved scroll/focus or steal focus from the newly active pane.
paneState.focus.set(pane.id, { node: inside });
const switchedDirect = dispatchFrom('inside');
document.activeElement = outside;
paneView({ selected: 35 }, switchedDirect);
assert.equal(paneBody.scrollTop, 500);
assert.equal(paneState.focus.size, 1);
assert.equal(document.activeElement, outside);
document.activeElement = inside;
// A settled direct action does not grant background views sticky ownership.
paneView({ selected: 32 });
assert.equal(paneBody.scrollTop, 0);
assert.equal(document.activeElement, heading);
// Even a newer pending direct action does not authorize a null-ack background
// context change. It remains pending until its own acknowledgement arrives.
paneBody.scrollTop = 300; document.activeElement = inside;
const backgroundPending = dispatchFrom('inside');
paneView({ selected: 36 });
assert.equal(paneBody.scrollTop, 0);
assert.equal(document.activeElement, heading);
assert.equal(vm.runInContext('pendingActionOrigin.sequence', context), backgroundPending);
paneView({ selected: 37 }, backgroundPending);
assert.equal(vm.runInContext('pendingActionOrigin', context), null);
// Newer action in another pane wins; old direct acknowledgements never steal it.
paneBody.scrollTop = 400; document.activeElement = inside;
const olderDirect = dispatchFrom('inside');
const newerExternal = dispatchFrom('outside');
document.activeElement = outside;
paneView({ selected: 33 }, olderDirect);
assert.equal(paneBody.scrollTop, 0);
assert.equal(document.activeElement, outside);
paneView({ selected: 34 }, newerExternal);
assert.equal(document.activeElement, outside);
document.activeElement = body;
// Explicit newer keyboard tree navigation wins over a pending external view;
// unlike automatic pane restoration, focusRow updates only pending ownership.
navigated = { isConnected: true, tabIndex: -1,
  closest(selector) { return selector === '[data-pane]' ? pane : selector === '[role="tree"]' ? navigationTree : null; },
  focus() { document.activeElement = navigated; },
  scrollIntoView() { paneBody.scrollTop = 450; },
};
const navigationTree = { querySelectorAll() { return [navigated]; } };
context.navigated = navigated;
const keyboardPending = dispatchFrom('outside');
const sentBeforeNavigation = workers[0].sent.length;
vm.runInContext('focusRow(navigated)', context);
assert.equal(workers[0].sent.length, sentBeforeNavigation, 'keyboard focus does not dispatch an invented event');
assert.equal(vm.runInContext('pendingActionOrigin.sequence', context), keyboardPending);
paneView({ selected: 38 }, keyboardPending);
assert.equal(document.activeElement, navigated);
assert.equal(paneBody.scrollTop, 450);
assert.equal(vm.runInContext('pendingActionOrigin', context), null);
paneView({ selected: 380 });
assert.equal(document.activeElement, heading, 'consumed keyboard ownership cannot exempt a background view');
assert.equal(paneBody.scrollTop, 0);
const keyboardAwayPending = dispatchFrom('outside');
vm.runInContext('focusRow(navigated)', context);
document.activeElement = outside;
paneView({ selected: 39 }, keyboardAwayPending);
assert.equal(document.activeElement, outside);
assert.equal(paneBody.scrollTop, 450);
assert.equal(vm.runInContext('pendingActionOrigin', context), null);
document.activeElement = body;
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
paneState.active = 'other-pane';
paneView({ selected: 4 }, null, 1);
assert.equal(paneState.active, pane.id, 'model reveal opens the requested narrow pane');
paneState.active = 'other-pane';
paneView({ selected: 4 }, null, 1);
assert.equal(paneState.active, 'other-pane', 'same reveal does not undo a later manual switch');
paneView({ selected: 4 }, null, 2);
assert.equal(paneState.active, pane.id, 'repeated selection can explicitly reveal the same content');
document.activeElement = inside;
paneBody.scrollTop = 300;
const internalReveal = dispatchFrom('inside');
paneView({ selected: 5 }, internalReveal, 3);
assert.equal(document.activeElement, inside, 'direct narrow-pane selection retains its focused control');
assert.equal(paneBody.scrollTop, 300, 'direct narrow-pane selection retains its scroll');
const internalBeforeSwitch = dispatchFrom('inside');
paneState.active = 'other-pane';
document.activeElement = outside;
paneView({ selected: 6 }, internalBeforeSwitch, 4);
assert.equal(paneState.active, 'other-pane', 'late internal selection does not undo a newer manual pane switch');
assert.equal(document.activeElement, outside);

paneState.resetScroll.add(pane.id);
dispatchFrom('inside');
vm.runInContext('clearProtected()', context);
assert.equal(paneState.resetScroll.size, 0, 'protected lifecycle clears pending resets');
assert.equal(paneState.context.size, 0);
assert.equal(vm.runInContext('pendingActionOrigin', context), null, 'protected lifecycle clears action origin');
vm.runInContext("layouts.delete('collection-layout')", context);
workers[0].sent.length = 0;
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


// Presentation-only tool exclusivity is reusable and never posts an application event.
context.toolFixture = {
  panes: [
    { id: 'main', dataset: { pane: 'main' } },
    { id: 'nav', dataset: { pane: 'leading' } },
    { id: 'related', dataset: { pane: 'trailing' } },
    { id: 'errors', dataset: { pane: 'bottom' } },
  ],
  singleSecondary: true, open: new Set(['main', 'nav', 'related']),
};
vm.runInContext("openPane(toolFixture, toolFixture.panes[3])", context);
assert.deepEqual([...context.toolFixture.open], ['main', 'nav', 'errors']);
vm.runInContext("openPane(toolFixture, toolFixture.panes[2])", context);
assert.deepEqual([...context.toolFixture.open], ['main', 'nav', 'related']);
assert.deepEqual([...vm.runInContext("defaultPanes(toolFixture.panes, true)", context)], ['main', 'nav', 'related']);
assert.deepEqual([...vm.runInContext("defaultPanes(toolFixture.panes, false)", context)], ['main', 'nav', 'related', 'errors']);

// Exact model-selected source windows use safe text and bounded numbered rows.
const sourceMount = sourceNode('pre');
sourceMount.id = 'document-source';
extraElements.set(sourceMount.id, sourceMount);
sourceElements.push(sourceMount);
const sourceBinding = { id: sourceMount.id, context: { report: 'owned', entry: 2, declaration: 4 },
  text: '<script>unsafe()</script>\nfn checked() {}\n', first_line: 90, first_column: 0,
  total_lines: 500, target_span: { start: { line: 91, column: 0 }, end: { line: 92, column: 0 } },
};
function sourceView(binding, sequence = null) {
  workers.at(-1).onmessage({ data: { kind: 'view', presentation_sequence: sequence, view: { sources: [binding] } } });
}
sourceView(sourceBinding);
assert.equal(sourceMount.children.length, 2);
assert.equal(sourceMount.children[0].children[1].textContent, '<script>unsafe()</script>');
assert.equal(sourceMount.children[0].dataset.line, '90');
assert.equal(sourceMount.children[0].dataset.highlight, 'false');
assert.equal(sourceMount.children[1].dataset.highlight, 'true');
assert.equal(sourceMount.children[1].children[0].scrolled, true, 'accepted target becomes visible');
assert.equal(sourceMount.scrollTop, 26, 'selected declaration starts at the source window top');
assert.equal(sourceMount.children[1].getBoundingClientRect().top, sourceMount.getBoundingClientRect().top + sourceMount.clientTop);

const stableSourceRow = sourceMount.children[0];
sourceMount.scrollLeft = 120;
sourceView(sourceBinding);
assert.equal(sourceMount.children[0], stableSourceRow, 'unrelated views preserve source nodes');
assert.equal(sourceMount.scrollLeft, 120, 'unrelated views preserve source horizontal position');
assert.equal(sourceMount.scrollTop, 26, 'unrelated views preserve source vertical position');
sourceView({ ...sourceBinding, text: Array.from({ length: 256 }, (_, i) => 'line ' + (257 + i)).join('\n'),
  first_line: 257, target_span: { start: { line: 300, column: 0 }, end: { line: 310, column: 1 } } });
assert.equal(sourceMount.children[43].dataset.line, '300');
assert.equal(sourceMount.scrollTop, 866, 'a late declaration aligns internally before outer nearest scrolling');
assert.equal(sourceMount.children[43].getBoundingClientRect().top, sourceMount.getBoundingClientRect().top + sourceMount.clientTop);

sourceView({ ...sourceBinding, text: 'continued', first_column: 64, target_span: null });
assert.equal(sourceMount.children[0].children[0].textContent, '90↪');
sourceView({ ...sourceBinding, text: '', target_span: null });
assert.equal(sourceMount.children.length, 0, 'withdrawn source clears protected pixels');
sourceView(sourceBinding);
lifecycle.get('pagehide')();
assert.equal(sourceMount.children.length, 0, 'page teardown clears source nodes synchronously');
lifecycle.get('pageshow')();
sourceView(sourceBinding);
assert.equal(sourceMount.children.length, 2, 'new instance does not inherit a stale render cache');
// Background source display must not pull a still-valid focused navigation row
// away from the viewport. The source window still updates to accepted contents.
sourceMount.closest = selector => selector === '[data-pane]' ? pane : selector === '[data-source-view]' ? sourceMount : null;
context.inside = inside;
vm.runInContext("focusKeyByElement.set(inside, 'current-analysis-row')", context);
document.activeElement = inside;
sourceView({ ...sourceBinding, text: 'updated source' });
assert.equal(sourceMount.children[0].children[0].scrolled, false);
assert.equal(sourceMount.children[0].children[1].textContent, 'updated source');
assert.equal(document.activeElement, inside);
document.activeElement = body;
sourceView(sourceBinding);
assert.equal(sourceMount.children[1].children[0].scrolled, true, 'external source navigation still reveals its target');
// External reveal must leave focus on the displayed code, not its heading above
// a vertically scrolled short viewport. Gutter anchors do not add Tab stops.
context.paneState = paneState;
vm.runInContext("layouts.set('collection-layout', paneState)", context);
paneState.narrow.matches = true;
paneState.active = 'other-pane';
document.activeElement = outside;
workers.at(-1).onmessage({ data: { kind: 'view', view: {
  sources: [{ ...sourceBinding, text: 'external replacement\nselected line' }],
  panes: [{ id: pane.id, context: { selected: 99 }, reveal: 99 }],
} } });
const focusedSourceLine = sourceMount.children[1].children[0];
assert.equal(document.activeElement, focusedSourceLine, 'external reveal focuses the visible source line');
assert.equal(focusedSourceLine.scrolled, true);
assert.equal(focusedSourceLine.tabIndex, -1, 'programmatic line focus adds no extra Tab stop');
assert.equal(focusedSourceLine['aria-label'], 'Source line 91');
// When a source response arrives after loading, a waiting pane-heading focus
// follows the new source line, while explicit user navigation keeps ownership.
document.activeElement = heading;
sourceView({ ...sourceBinding, text: 'loaded replacement\nselected line' });
assert.equal(document.activeElement, sourceMount.children[1].children[0]);
const pagingControl = { ...inside, disabled: false };
context.pagingControl = pagingControl;
vm.runInContext("actionByElement.set(pagingControl, { kind: 'next_window' })", context);
document.activeElement = pagingControl;
sourceMount.scrollTop = 140;
const nextWindow = dispatchFrom('pagingControl');
sourceView({ ...sourceBinding, text: 'next window', first_line: 200, target_span: null }, nextWindow);
assert.equal(document.activeElement, pagingControl, 'line paging retains an enabled control and viewport');
assert.equal(sourceMount.children[0].children[0].scrolled, false, 'line paging does not scroll its control away');
assert.equal(sourceMount.scrollTop, 0, 'new source window resets its own inner scroll');
const lastWindow = dispatchFrom('pagingControl');
pagingControl.disabled = true;
sourceView({ ...sourceBinding, text: 'last window', first_line: 400, target_span: null }, lastWindow);
assert.equal(document.activeElement, sourceMount.children[0].children[0], 'exhausted paging control moves focus to visible source');
vm.runInContext("layouts.delete('collection-layout')", context);
console.log('Source presentation: safe text, line spans, continuation, stable scroll, narrow reveal and teardown passed.');
