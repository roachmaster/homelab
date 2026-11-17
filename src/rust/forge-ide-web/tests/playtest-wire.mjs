// Verify the wire contract against the compiled Rust/WebAssembly playtest.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const path = process.argv[2];
assert.ok(path, 'supply the built forge_ide_web.wasm path');
const module = await WebAssembly.compile(await readFile(path));
const game = () => new WebAssembly.Instance(module, {}).exports;
const w = game();

const BOARD = 32;
const ENTITIES = 4128;
const EVENTS = 9248;
const STATS = 10272;
const TRACE_RECORDS = 24;
const ascii = (view, offset, size) =>
  String.fromCharCode(...new Uint8Array(view.buffer, view.byteOffset + offset, size));
const board = (view, x, y) =>
  view.getUint8(BOARD + y * view.getUint16(6, true) + x);

function wire(exports, type, expectedLength, magic) {
  const address = exports[`forge_playtest_${type}`]() >>> 0;
  const length = exports[`forge_playtest_${type}_len`]();
  assert.equal(length, expectedLength);
  assert.ok(address > 0);
  assert.ok(address + length <= exports.memory.buffer.byteLength);
  const view = new DataView(exports.memory.buffer, address, length);
  assert.equal(ascii(view, 0, 4), magic);
  assert.equal(view.getUint16(4, true), 1);
  return view;
}

const snapshot = (exports = w) => wire(exports, 'snapshot', 10284, 'FGS1');
const trace = (exports = w) => wire(exports, 'trace', 8216, 'FGT1');
const tick = (view) => view.getUint32(20, true);
const epoch = (view) => view.getUint32(16, true);
const state = (view) => view.getUint8(28);

function tap(exports, action) {
  exports.forge_playtest_input(action, 1);
  exports.forge_playtest_input(action, 0);
}

assert.equal(w.abi_version(), 4, 'the existing host ABI remains compatible');
const initial = snapshot();
const snapshotAddress = initial.byteOffset;
const traceAddress = trace().byteOffset;
assert.equal(initial.getUint16(6, true), 13);
assert.equal(initial.getUint16(8, true), 9);
assert.equal(initial.getUint16(10, true), 4);
assert.equal(initial.getUint16(12, true), 0);
assert.equal(initial.getUint16(30, true), 0);
assert.equal(tick(initial), 0);
assert.equal(epoch(initial), 1);
assert.equal(board(initial, 1, 1), 5);
assert.equal(board(initial, 1, 7), 2);
const startingKey = Array.from({length: 4}, (_, i) => ENTITIES + i * 20)
  .find(offset => initial.getInt32(offset + 4, true) === 256 &&
    initial.getInt32(offset + 8, true) === 256);
assert.notEqual(startingKey, undefined, 'the key is published in y-up Q8 tiles');
const keyId = initial.getUint16(startingKey, true);

for (let y = 0; y < w.forge_playtest_height(); y++) {
  for (let x = 0; x < w.forge_playtest_width(); x++) {
    const cell = board(initial, x, y);
    assert.equal(w.forge_playtest_cell(x, y), cell === 6 ? 1 : cell);
  }
}

// These elapsed times would advance two ticks if stepping reused the accumulator.
w.forge_playtest_advance(33_333);
assert.equal(snapshot().getUint32(24, true), 999_980);
const before = tick(snapshot());
tap(w, 3);
assert.equal(snapshot().getUint16(14, true), 2);
w.forge_playtest_step();
assert.equal(tick(snapshot()), before + 1);
assert.equal(snapshot().getUint8(29), 1);
assert.equal(board(snapshot(), 2, 1), 5);

const priorEpoch = epoch(snapshot());
w.forge_playtest_restart();
assert.equal(epoch(snapshot()), priorEpoch + 1);
assert.equal(tick(snapshot()), 0);
assert.equal(snapshot().byteOffset, snapshotAddress);
assert.equal(trace().byteOffset, traceAddress);
assert.equal(trace().getUint32(8, true), epoch(snapshot()));
assert.equal(board(snapshot(), 1, 1), 5);
assert.equal(board(snapshot(), 1, 7), 2);

const action = { U: 0, D: 1, L: 2, R: 3 };
for (const direction of 'RRDDLLDDRRDDLL') {
  tap(w, action[direction]);
  w.forge_playtest_step();
}
const foundKey = snapshot();
assert.equal(state(foundKey), 1);
assert.ok(Array.from({length: foundKey.getUint16(12, true)}, (_, i) => EVENTS + i * 16)
  .some(offset => foundKey.getUint16(offset + 8, true) === keyId &&
    foundKey.getUint16(offset + 12, true) === 1 && foundKey.getUint16(offset + 14, true) === 1),
  'delivered key events publish the stable ID and the occurrence tile, even after despawn');
for (const direction of 'RRUURRDDRRRRRRUUU') {
  tap(w, action[direction]);
  w.forge_playtest_step();
}
assert.equal(state(snapshot()), 2);

const recorded = trace();
assert.equal(recorded.getUint8(23), 1);
const inputs = new Map();
for (let i = 0; i < recorded.getUint16(6, true); i++) {
  const offset = TRACE_RECORDS + i * 16;
  if (recorded.getUint8(offset + 9) === 0) {
    const appliedTick = recorded.getUint32(offset + 4, true);
    assert.equal(inputs.has(appliedTick), false);
    inputs.set(appliedTick, recorded.getUint32(offset + 12, true));
  }
}
const replay = game();
const final = snapshot();
for (let next = 1; next <= tick(final); next++) {
  if (inputs.has(next)) tap(replay, inputs.get(next));
  replay.forge_playtest_step();
}
const reproduced = snapshot(replay);
assert.equal(state(reproduced), 2);
assert.equal(tick(reproduced), tick(final));
assert.deepEqual(
  new Uint8Array(reproduced.buffer, reproduced.byteOffset + BOARD, 117),
  new Uint8Array(final.buffer, final.byteOffset + BOARD, 117),
  'applied-tick capture reproduces the full board',
);

w.forge_playtest_restart();
w.forge_playtest_advance(5_000_000);
assert.equal(snapshot().getUint8(29), 4);
assert.equal(snapshot().getUint32(STATS, true), 296, 'loss exceeds an 8-bit counter');
w.forge_playtest_trace_channels(0);
tap(w, 3);
w.forge_playtest_step();
assert.equal(trace().getUint8(23), 0, 'filtered movement invalidates the capture');
w.forge_playtest_trace_channels(0x3f);
assert.equal(trace().getUint8(23), 0, 'reenabling capture cannot recover the missing input');
w.forge_playtest_restart();
for (let i = 0; i < 520; i++) w.forge_playtest_advance(250_000);
const wrapped = trace();
assert.equal(wrapped.getUint16(6, true), 512);
assert.ok(wrapped.getUint32(16, true) > 0);
assert.equal(wrapped.getUint8(23), 0, 'ring overflow invalidates the capture');
assert.ok(wrapped.getUint32(TRACE_RECORDS, true) > 1);
assert.equal(
  wrapped.getUint32(TRACE_RECORDS + 511 * 16, true),
  wrapped.getUint32(12, true),
);

const oldMemory = w.memory.buffer;
w.memory.grow(1);
assert.notEqual(w.memory.buffer, oldMemory);
assert.equal(snapshot().byteOffset, snapshotAddress);
assert.equal(trace().byteOffset, traceAddress);
console.log('Playtest WASM: bounded binary snapshots and trace, step, restart, loss markers and replay pass.');
