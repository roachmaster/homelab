// Verify the wire contract against the compiled Rust/WebAssembly playtest.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
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

function wire(exports, type, expectedLength, magic, versioned = true) {
  const address = exports[`forge_playtest_${type}`]() >>> 0;
  const length = exports[`forge_playtest_${type}_len`]();
  assert.equal(length, expectedLength);
  assert.ok(address > 0);
  assert.ok(address + length <= exports.memory.buffer.byteLength);
  const view = new DataView(exports.memory.buffer, address, length);
  assert.equal(ascii(view, 0, 4), magic);
  if (versioned) assert.equal(view.getUint16(4, true), 1);
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

const levelSnapshot = (exports) => wire(exports, 'level_snapshot', 48, 'FGL1', false);
const poseSnapshot = (exports) => wire(exports, 'pose_snapshot', 32, 'FGP1', false);
const loadFixture = async (file) =>
  JSON.parse(await readFile(new URL(`../../../python/${file}`, import.meta.url), 'utf8'));
const scout = await loadFixture('pixel-art/examples/room-one-scout.json');
const timing = Object.fromEntries(['idle', 'walk', 'jump', 'crouch'].map(name =>
  [name, scout.clips[name].map(frame => frame.duration_ms)]));
const collider = { left: -10240, top: -53248, right: 18432, bottom: 0 };
const traversal = { ...await loadFixture('game-assets/examples/levels/traversal-yard.json'), timing, collider };
const roomOne = { ...await loadFixture('game-assets/examples/levels/room-one.json'), timing, collider };
const manifestDigest = createHash('sha256').update('playtest-wire fixture manifest v1').digest();
const manifestTag = manifestDigest.readUInt32LE(0);
const copyBytes = (view) =>
  Uint8Array.from(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));

function submitManifest(exports, request) {
  for (let index = 0; index < 8; index++) {
    assert.equal(exports.forge_playtest_level_manifest_word(
      request, index, manifestDigest.readUInt32LE(index * 4),
    ), 1, 'all eight words of the manifest identity are admitted');
  }
}

function prepareLevel(exports, request, level) {
  assert.equal(exports.forge_playtest_level_begin(request, manifestTag), 1);
  submitManifest(exports, request);
  for (const byte of Buffer.from(JSON.stringify(level))) {
    assert.equal(exports.forge_playtest_level_byte(request, byte), 1);
  }
  return exports.forge_playtest_level_prepare(request);
}

function playerPose(exports) {
  const published = snapshot(exports);
  const pose = poseSnapshot(exports);
  let offset = ENTITIES;
  while (offset < ENTITIES + published.getUint16(10, true) * 20 && published.getUint8(offset + 2) !== 1) {
    offset += 20;
  }
  assert.ok(offset < ENTITIES + published.getUint16(10, true) * 20, 'the player has an entity record');
  assert.equal(pose.getUint32(4, true), epoch(published));
  assert.equal(pose.getUint32(8, true), tick(published));
  assert.equal(pose.getUint16(12, true), published.getUint16(offset, true));
  assert.equal(pose.getUint16(12, true), traversal.spawns[0].id);
  return {
    x: published.getInt32(offset + 4, true),
    y: published.getInt32(offset + 8, true),
    previousY: published.getInt32(offset + 16, true),
    phase: pose.getUint8(14),
    frame: pose.getUint32(16, true),
    grounded: (pose.getUint8(20) & 1) !== 0,
    contacts: pose.getUint8(21),
  };
}

const authored = game();
assert.equal(authored.forge_playtest_level(), 0);
assert.equal(authored.forge_playtest_level_begin(0, manifestTag), 0, 'generation zero is reserved');
authored.forge_playtest_step();
const beforePrepare = copyBytes(snapshot(authored));
const beforePose = copyBytes(poseSnapshot(authored));
assert.equal(prepareLevel(authored, 1, traversal), 1);
assert.deepEqual(copyBytes(snapshot(authored)), beforePrepare, 'preparation changes no active snapshot bytes');
assert.deepEqual(copyBytes(poseSnapshot(authored)), beforePose, 'preparation changes no active pose bytes');
authored.forge_playtest_advance(0);
assert.equal(authored.forge_playtest_level(), 0, 'a zero-tick call cannot activate a level');
assert.deepEqual(copyBytes(poseSnapshot(authored)), beforePose);
authored.forge_playtest_advance(16_666);
assert.equal(authored.forge_playtest_level(), 0, 'sub-tick time cannot activate a level');
assert.equal(epoch(snapshot(authored)), 1);
assert.equal(tick(snapshot(authored)), 1);
assert.equal(board(snapshot(authored), 1, 1), 5);
authored.forge_playtest_advance(1);
assert.equal(authored.forge_playtest_level(), 2);
const activation = levelSnapshot(authored);
assert.equal(activation.getUint32(4, true), 2);
assert.equal(activation.getUint32(8, true), 2, 'activation records the next tick of the prior world');
assert.equal(activation.getUint32(12, true), 2);
assert.deepEqual(copyBytes(activation).slice(16), Uint8Array.from(manifestDigest));
assert.equal(epoch(snapshot(authored)), 2);
assert.equal(tick(snapshot(authored)), 0, 'the new epoch starts at its initial state');
assert.equal(snapshot(authored).getUint16(8, true), traversal.height);
const initialPose = playerPose(authored);
assert.equal(initialPose.grounded, true);
assert.equal(initialPose.phase, 0);
assert.equal(initialPose.contacts, 0);

const divided = game();
assert.equal(prepareLevel(divided, 1, traversal), 1);
divided.forge_playtest_step();
tap(authored, 4);
tap(divided, 4);
const jump = [];
for (let i = 1; i <= 35; i++) {
  authored.forge_playtest_step();
  divided.forge_playtest_advance(16_666);
  divided.forge_playtest_advance(1);
  const pose = playerPose(authored);
  assert.deepEqual(playerPose(divided), pose, 'frame partitioning preserves physics and animation');
  assert.equal(tick(snapshot(divided)), i);
  assert.equal(pose.x, initialPose.x);
  assert.equal(pose.previousY, i === 1 ? initialPose.y : jump.at(-1).y);
  jump.push(pose);
}
assert.equal(jump[0].phase, 2, 'the first jump tick publishes a rising pose');
assert.equal(jump[0].grounded, false);
assert.equal(jump[13].y - initialPose.y, Math.trunc(61_117 / 256), 'the discrete apex is reached on tick 14');
assert.equal(jump[14].phase, 3, 'apex crossing selects the falling pose');
assert.equal(jump[27].grounded, false);
assert.equal(jump[28].grounded, true);
assert.equal(jump[28].phase, 4);
assert.equal(jump[28].y, initialPose.y, 'the feet return to the floor on tick 29');
assert.deepEqual(jump.flatMap((pose, index) => (pose.contacts & 1) ? [index + 1] : []), [29]);
const landed = snapshot(authored);
assert.ok(Array.from({ length: landed.getUint16(12, true) }, (_, i) => EVENTS + i * 16)
  .some(offset => landed.getUint8(offset + 10) === 5 && landed.getUint32(offset + 4, true) === 29),
  'the event ring publishes the landing on the same tick as the pose');

function startTraversal() {
  const exports = game();
  assert.equal(prepareLevel(exports, 1, traversal), 1);
  exports.forge_playtest_step();
  exports.forge_playtest_input(3, 1);
  return exports;
}
function stepTicks(exports, count) {
  for (let index = 0; index < count; index++) exports.forge_playtest_step();
}

const lockedTraversal = startTraversal();
stepTicks(160);
const gate = traversal.spawns.find(spawn => spawn.kind === 'exit');
assert.equal(state(snapshot(lockedTraversal)), 0, 'walking under the raised key cannot unlock the exit');
assert.equal(board(snapshot(lockedTraversal), gate.x, gate.y), 3);
assert.ok(playerPose(lockedTraversal).x < gate.x * 256, 'the locked exit blocks the player');

const traversed = startTraversal();
stepTicks(traversed, 60);
tap(traversed, 4);
stepTicks(traversed, 30);
assert.equal(state(snapshot(traversed)), 1, 'collecting the raised key during the jump opens the exit');
stepTicks(traversed, 60);
const wonTraversal = snapshot(traversed);
assert.equal(state(wonTraversal), 2, 'the cooked Traversal Yard is playable through its win condition');
const objectives = Array.from({ length: wonTraversal.getUint16(12, true) }, (_, index) =>
  wonTraversal.getUint8(EVENTS + index * 16 + 10)).filter(kind => kind >= 1 && kind <= 3);
assert.deepEqual(objectives, [1, 2, 3], 'key, unlock, and win publish once in order');

assert.equal(authored.forge_playtest_level_begin(2, manifestTag), 1);
assert.equal(authored.forge_playtest_level_byte(2, 123), 1);
assert.equal(prepareLevel(authored, 3, roomOne), 1, 'the newer request replaces pending bytes');
assert.equal(authored.forge_playtest_level_begin(2, manifestTag), 0);
assert.equal(authored.forge_playtest_level_manifest_word(2, 0, manifestTag), 0);
assert.equal(authored.forge_playtest_level_byte(2, 125), 0);
assert.equal(authored.forge_playtest_level_prepare(2), 0);
authored.forge_playtest_step();
assert.equal(authored.forge_playtest_level(), 1, 'stale traffic cannot displace a newer prepared room');
assert.equal(tick(snapshot(authored)), 0);
assert.equal(board(snapshot(authored), 1, 1), 5);

assert.equal(prepareLevel(authored, 4, { ...traversal, unknown: true }), 0);
authored.forge_playtest_step();
assert.equal(authored.forge_playtest_level(), 1, 'invalid content retains the active room');
assert.equal(epoch(snapshot(authored)), 3);
assert.equal(tick(snapshot(authored)), 1);
assert.equal(authored.forge_playtest_level_begin(5, manifestTag), 1);
submitManifest(authored, 5);
for (let count = 0; count < 16 * 1024; count++) {
  assert.equal(authored.forge_playtest_level_byte(5, 32), 1);
}
assert.equal(authored.forge_playtest_level_byte(5, 32), 0, 'the WASM byte ingress has a hard limit');
assert.equal(authored.forge_playtest_level_prepare(5), 0);
assert.equal(authored.forge_playtest_level_begin(6, manifestTag), 1);
assert.equal(authored.forge_playtest_level_manifest_word(6, 0, manifestTag ^ 1), 0);
assert.equal(authored.forge_playtest_level_prepare(6), 0);

assert.equal(prepareLevel(authored, 7, traversal), 1);
authored.forge_playtest_restart();
authored.forge_playtest_step();
assert.equal(authored.forge_playtest_level(), 1, 'restart cancels a prepared level swap');
assert.equal(epoch(snapshot(authored)), 4);
assert.equal(tick(snapshot(authored)), 1);
assert.equal(levelSnapshot(authored).getUint32(8, true), 0);
assert.equal(authored.forge_playtest_level_prepare(7), 0);
console.log('Playtest WASM: cooked admission, generation changes, pose identity, 29-tick jump and Traversal Yard win pass.');
