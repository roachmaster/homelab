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

<<<<<<< HEAD
assert.equal(w.abi_version(), 10, 'the current atomic UI bundle ABI is used');
=======
assert.equal(w.abi_version(), 12, 'the current atomic UI bundle ABI is used');
>>>>>>> a1ee6f64ee377a6c3640b86d70de5ece8373aacc
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

const cookedLevelBytes = new Map([
  [1, Buffer.from(JSON.stringify(roomOne))], [2, Buffer.from(JSON.stringify(traversal))],
]);
const cookedManifest = Buffer.from(JSON.stringify({
  version: 1,
  entries: Object.fromEntries([[1, 'room-one'], [2, 'traversal-yard']].map(([slot, id]) => [id, {
    kind: 'level', path: `levels/${id}.json`, dependencies: [], optional_dependencies: ['scout-atlas'],
    sha256: createHash('sha256').update(cookedLevelBytes.get(slot)).digest('hex'),
  }])),
}));
const cookedManifestDigest = createHash('sha256').update(cookedManifest).digest();

function uploadAsset(exports, asset, request, bytes) {
  const pointer = exports.forge_playtest_asset_reserve(asset, bytes.byteLength) >>> 0;
  assert.ok(pointer, 'Rust allocates a bounded ingress buffer');
  new Uint8Array(exports.memory.buffer, pointer, bytes.byteLength).set(bytes);
  return exports.forge_playtest_asset_accept(asset, request);
}

function acceptVerifiedManifest(exports, bytes = cookedManifest) {
  return uploadAsset(exports, 0, 0, bytes);
}

function prepareVerifiedLevel(exports, slot, bytes = cookedLevelBytes.get(slot)) {
  const request = exports.forge_playtest_level_request(slot);
  assert.ok(request, 'Rust issues the request generation');
  return uploadAsset(exports, slot, request, bytes);
}

const verified = game();
assert.equal(verified.forge_playtest_asset_reserve(2, 16 * 1024 + 1), 0);
assert.equal(acceptVerifiedManifest(verified, Buffer.from('{')), 0);
assert.equal(prepareVerifiedLevel(verified, 2), 0, 'a level needs an admitted manifest');
const supersededManifest = verified.forge_playtest_level_request(1);
const manifestRequest = verified.forge_playtest_level_request(2);
assert.equal(verified.forge_playtest_level_read(supersededManifest), 0);
assert.equal(verified.forge_playtest_level_read(manifestRequest), 1, 'Rust requests the manifest first');
assert.equal(uploadAsset(verified, 0, supersededManifest, cookedManifest), 0);
assert.equal(uploadAsset(verified, 0, manifestRequest, cookedManifest), 1);
assert.equal(verified.forge_playtest_level_read(manifestRequest), 3, 'Rust now requests Traversal Yard');
assert.equal(verified.forge_playtest_atlas_read(), 4, 'the atlas reuses the verified manifest');
assert.equal(acceptVerifiedManifest(verified, Buffer.concat([cookedManifest, Buffer.from(' ')])), 0);
assert.equal(prepareVerifiedLevel(verified, 2, Buffer.concat([cookedLevelBytes.get(2), Buffer.from(' ')])), 0,
  'Rust rejects valid level JSON whose bytes do not match the manifest digest');
const stale = verified.forge_playtest_level_request(2);
const newest = verified.forge_playtest_level_request(1);
assert.equal(uploadAsset(verified, 2, stale, cookedLevelBytes.get(2)), 0);
assert.equal(uploadAsset(verified, 2, newest, cookedLevelBytes.get(2)), 0,
  'a newer request cannot admit another level');
assert.equal(uploadAsset(verified, 1, newest, cookedLevelBytes.get(1)), 1);
verified.forge_playtest_step();
assert.equal(verified.forge_playtest_level(), 1);
assert.deepEqual(copyBytes(levelSnapshot(verified)).slice(16), Uint8Array.from(cookedManifestDigest));
assert.deepEqual([0, 1, 2, 3].map(index => verified.forge_playtest_level_collider(index)),
  Object.values(collider));

// The authored Room One chunk and the builtin Rust const describe the same
// room. The loaded snapshot must match a fresh builtin instance on the board
// and the entity records; only the epoch differs, because activation starts
// a new epoch. If either side changes without the other, this fails.
const builtin = game();
const builtinSnapshot = snapshot(builtin);
const loadedSnapshot = snapshot(verified);
assert.equal(tick(loadedSnapshot), 0, 'the loaded room starts at its initial state');
assert.equal(epoch(builtinSnapshot), 1);
assert.equal(epoch(loadedSnapshot), 2, 'activation starts a new epoch');
assert.equal(state(loadedSnapshot), state(builtinSnapshot));
assert.equal(loadedSnapshot.getUint16(6, true), builtinSnapshot.getUint16(6, true));
assert.equal(loadedSnapshot.getUint16(8, true), builtinSnapshot.getUint16(8, true));
assert.equal(loadedSnapshot.getUint16(10, true), builtinSnapshot.getUint16(10, true));
assert.deepEqual(
  new Uint8Array(loadedSnapshot.buffer, loadedSnapshot.byteOffset + BOARD, 117),
  new Uint8Array(builtinSnapshot.buffer, builtinSnapshot.byteOffset + BOARD, 117),
  'the authored chunk and the builtin const publish the same board',
);
assert.deepEqual(
  new Uint8Array(loadedSnapshot.buffer, loadedSnapshot.byteOffset + ENTITIES, 80),
  new Uint8Array(builtinSnapshot.buffer, builtinSnapshot.byteOffset + ENTITIES, 80),
  'the authored chunk and the builtin const publish the same entities',
);
const cancelled = verified.forge_playtest_level_request(2);
verified.forge_playtest_level_fail(cancelled);
assert.equal(uploadAsset(verified, 2, cancelled, cookedLevelBytes.get(2)), 0,
  'an I/O failure invalidates a later result for that request');
console.log('Playtest WASM: Rust verifies raw asset digests, request generations and active collider; the authored Room One agrees with the builtin const.');

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

function sessionBytes(exports) {
  const pointer = exports.forge_playtest_replay() >>> 0;
  const length = exports.forge_playtest_replay_len();
  assert.ok(pointer && length);
  return Uint8Array.from(new Uint8Array(exports.memory.buffer, pointer, length));
}

function readSession(exports) {
  return JSON.parse(new TextDecoder().decode(sessionBytes(exports)));
}

const recordedGame = game();
assert.equal(acceptVerifiedManifest(recordedGame), 1);
recordedGame.forge_playtest_trace_channels(0);
recordedGame.forge_playtest_step();
const builtInState = copyBytes(snapshot(recordedGame));
assert.equal(prepareVerifiedLevel(recordedGame, 1), 1);
recordedGame.forge_playtest_step();
tap(recordedGame, 3);
recordedGame.forge_playtest_step();
recordedGame.forge_playtest_input(3, 1);
recordedGame.forge_playtest_set_paused(1);
recordedGame.forge_playtest_advance(250_000);
assert.equal(tick(snapshot(recordedGame)), 1, 'paused time advances no ticks');
assert.equal(snapshot(recordedGame).getUint16(14, true), 0, 'pausing clears pending input');
recordedGame.forge_playtest_step();
assert.equal(tick(snapshot(recordedGame)), 2, 'manual step remains available while paused');
recordedGame.forge_playtest_set_paused(0);
const roomOneState = copyBytes(snapshot(recordedGame));
assert.equal(prepareVerifiedLevel(recordedGame, 2), 1);
recordedGame.forge_playtest_step();
recordedGame.forge_playtest_input(3, 1);
stepTicks(recordedGame, 60);
tap(recordedGame, 4);
stepTicks(recordedGame, 30);
recordedGame.forge_playtest_input(3, 0);
recordedGame.forge_playtest_step();
const traversalState = copyBytes(snapshot(recordedGame));
const traversalPose = copyBytes(poseSnapshot(recordedGame));

const journal = readSession(recordedGame);
assert.equal(journal.version, 1);
assert.equal(journal.complete, true, 'the session recorder is independent of the trace mask');
assert.deepEqual(journal.segments.map(segment => ({
  epoch: segment.epoch, slot: segment.level_slot, manifest: segment.manifest_sha256,
  tick: segment.final_tick, transition: segment.transition,
})), [
  { epoch: 1, slot: 0, manifest: null, tick: 1,
    transition: { kind: 'level', boundary_tick: 2, incoming_epoch: 2 } },
  { epoch: 2, slot: 1, manifest: cookedManifestDigest.toString('hex'), tick: 2,
    transition: { kind: 'level', boundary_tick: 3, incoming_epoch: 3 } },
  { epoch: 3, slot: 2, manifest: cookedManifestDigest.toString('hex'), tick: 91, transition: null },
]);
assert.deepEqual(journal.segments[2].inputs, [
  { input: 'platform-right', ticks: 60 }, { input: 'jump-right', ticks: 1 },
  { input: 'platform-right', ticks: 29 }, { input: 'platform-idle', ticks: 1 },
]);

const playback = game();
const active = copyBytes(snapshot(playback));
const importedBytes = sessionBytes(recordedGame);
const importReplay = (bytes) => {
  const address = playback.forge_playtest_replay_reserve(bytes.length) >>> 0;
  assert.ok(address, 'the replay has a Rust-owned input buffer');
  new Uint8Array(playback.memory.buffer, address, bytes.length).set(bytes);
  return playback.forge_playtest_replay_finish();
};
const importJournal = (data) => importReplay(Buffer.from(JSON.stringify(data)));
const replayView = (type, length) => Uint8Array.from(new Uint8Array(
  playback.memory.buffer, playback[`forge_playtest_replay_${type}`]() >>> 0, length,
));
assert.equal(importReplay(importedBytes), 0, 'unavailable cooked generations reject playback');
assert.equal(playback.forge_playtest_replay_snapshot(), 0);
assert.equal(acceptVerifiedManifest(playback), 1);
assert.equal(prepareVerifiedLevel(playback, 1), 1);
assert.equal(prepareVerifiedLevel(playback, 2), 1);
for (const [index, expected] of [builtInState, roomOneState].entries()) {
  const prefix = structuredClone(journal);
  prefix.segments.length = index + 1;
  prefix.segments.at(-1).transition = null;
  assert.equal(importJournal(prefix), 1);
  assert.deepEqual(replayView('snapshot', expected.length), expected);
}
assert.equal(importReplay(importedBytes), 1, 'Rust runs the complete exported replay');
assert.deepEqual(replayView('snapshot', traversalState.length), traversalState);
assert.deepEqual(replayView('pose', traversalPose.length), traversalPose);
assert.equal(playback.forge_playtest_replay_finish(), 0, 'finishing consumes the uploaded replay');
assert.equal(playback.forge_playtest_replay_reserve(2 * 1024 * 1024 + 1), 0);

for (const mutate of [
  data => { data.complete = false; },
  data => { data.segments[1].manifest_sha256 = '0'.repeat(64); },
  data => { data.segments[2].level_slot = 9; },
  data => { data.segments[0].transition.boundary_tick++; },
  data => { data.segments[0].transition.incoming_epoch++; },
  data => { data.segments[1].inputs[0].input = 'platform-right'; },
  data => { data.segments[1].inputs[0].ticks = 36_001; },
  data => { data.segments[1].final_tick++; },
  data => { delete data.segments[2].transition; },
]) {
  const invalid = structuredClone(journal);
  mutate(invalid);
  assert.equal(importJournal(invalid), 0, 'invalid replay fails without replacing results');
  assert.deepEqual(replayView('snapshot', traversalState.length), traversalState);
}
recordedGame.forge_playtest_restart();
recordedGame.forge_playtest_step();
assert.equal(importReplay(sessionBytes(recordedGame)), 1, 'Rust replays an authored restart');
assert.deepEqual(replayView('snapshot', traversalState.length), copyBytes(snapshot(recordedGame)));
assert.deepEqual(copyBytes(snapshot(playback)), active, 'playback preserves the active live room');

const beforeCollision = copyBytes(snapshot(recordedGame));
const collision = wire(recordedGame, 'collision_snapshot', 5216, 'FGC1', false);
assert.equal(collision.getUint32(4, true), epoch(snapshot(recordedGame)));
assert.equal(collision.getUint32(8, true), tick(snapshot(recordedGame)));
assert.equal(collision.getUint16(12, true), snapshot(recordedGame).getUint16(10, true));
assert.equal(collision.getUint8(36), 3, 'the downward probe sees the upward floor normal');
assert.deepEqual(copyBytes(snapshot(recordedGame)), beforeCollision, 'overlay queries do not advance the world');

const historical = game();
const liveBeforeArchive = copyBytes(snapshot(historical));
const journalInput = historical.forge_playtest_replay_reserve(importedBytes.length) >>> 0;
new Uint8Array(historical.memory.buffer, journalInput, importedBytes.length).set(importedBytes);
assert.equal(historical.forge_playtest_replay_prepare(), 1);
assert.equal(historical.forge_playtest_replay_need_count(), 2);
function archiveRequest() {
  const address = historical.forge_playtest_replay_archive_read() >>> 0;
  if (!address) return null;
  const view = new DataView(historical.memory.buffer, address, 44);
  assert.equal(view.getUint32(0, true), 0x31524746);
  const digest = Buffer.from(new Uint8Array(view.buffer, address + 12, 32));
  assert.deepEqual(digest, cookedManifestDigest);
  return { generation: view.getUint32(4, true), asset: view.getUint32(8, true) };
}
function acceptArchive(request, bytes, generation = request.generation) {
  const pointer = historical.forge_playtest_asset_reserve(request.asset, bytes.length) >>> 0;
  new Uint8Array(historical.memory.buffer, pointer, bytes.length).set(bytes);
  return historical.forge_playtest_replay_archive_accept(generation);
}
let previous = 0;
for (const asset of [0, 1, 2]) {
  const bytes = asset === 0 ? cookedManifest : cookedLevelBytes.get(asset);
  const request = archiveRequest();
  assert.equal(request.asset, asset, 'Rust requests the shared manifest only once');
  assert.deepEqual(archiveRequest(), request, 'an outstanding read can be inspected again');
  assert.equal(acceptArchive(request, bytes, previous), 0, 'stale responses cannot consume a read');
  assert.equal(acceptArchive(request, Buffer.concat([bytes, Buffer.from(' ')])), 0);
  const retry = archiveRequest();
  assert.equal(retry.asset, asset);
  assert.ok(retry.generation > request.generation);
  assert.equal(acceptArchive(retry, bytes), 1);
  previous = retry.generation;
}
assert.equal(archiveRequest(), null);
assert.equal(historical.forge_playtest_replay_finish(), 1);
assert.equal(historical.forge_playtest_replay_restart(), 1);
for (let step = 0; step < 100 && !historical.forge_playtest_replay_done(); step++) {
  assert.equal(historical.forge_playtest_replay_step(1), 1);
}
assert.equal(historical.forge_playtest_replay_done(), 1);
const archivedFrame = new Uint8Array(historical.memory.buffer,
  historical.forge_playtest_replay_snapshot() >>> 0, traversalState.length);
assert.deepEqual(archivedFrame, traversalState);
assert.deepEqual(copyBytes(snapshot(historical)), liveBeforeArchive);

const unverifiedGame = game();
assert.equal(prepareLevel(unverifiedGame, 1, roomOne), 1);
unverifiedGame.forge_playtest_step();
assert.equal(readSession(unverifiedGame).complete, false, 'legacy unverified assets cannot claim a replayable session');

const fullJournal = game();
for (let index = 0; index < 128; index++) fullJournal.forge_playtest_restart();
assert.equal(readSession(fullJournal).complete, false, 'segment budget exhaustion is explicit');
assert.equal(readSession(fullJournal).segments.length, 128);
fullJournal.forge_playtest_step();
assert.equal(tick(snapshot(fullJournal)), 1, 'simulation continues after the recorder reaches capacity');
console.log('Playtest WASM: pause gate, bounded session recorder, and two authored room replay pass.');
