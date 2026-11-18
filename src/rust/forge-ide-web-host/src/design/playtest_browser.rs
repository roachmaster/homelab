//! Build-time emitter for the Canvas/WebAssembly adapter.
//!
//! Rust supplies the page bindings and tile visuals. The browser adapter owns
//! device capture, presentation, and its wall clock. WASM owns game state.

use serde::Serialize;

#[path = "playtest_asset_browser.rs"]
mod assets;
#[path = "playtest_debug_browser.rs"]
mod debug;
#[path = "playtest_replay_browser.rs"]
mod replay;
#[path = "playtest_snapshot_browser.rs"]
mod snapshot;

type Point = [u8; 2];

#[derive(Serialize)]
#[serde(tag = "kind", content = "args", rename_all = "snake_case")]
enum Paint {
    FillRect(Point, Point, &'static str),
    StrokeRect(Point, Point, &'static str, u8),
    FillCircle(Point, u8, &'static str),
    StrokeArc(Point, u8, [f32; 2], &'static str, u8),
    StrokePath(&'static [Point], &'static str, u8),
}

#[derive(Serialize)]
struct Wasm {
    prefix: &'static str,
}

#[derive(Serialize)]
struct Controls {
    board: &'static str,
    title: &'static str,
    status: &'static str,
    level_status: &'static str,
    level_group: &'static str,
    instructions: &'static str,
    restart: &'static str,
    direction: &'static str,
    action_attribute: &'static str,
    held_attribute: &'static str,
    action_count: usize,
    jump_action: u8,
    keys: &'static [(&'static str, u8)],
}

#[derive(Serialize)]
struct LevelChoice {
    selector: &'static str,
    label: &'static str,
    level: u8,
}

#[derive(Serialize)]
struct Assets {
    levels: &'static [LevelChoice],
    worker_url: &'static str,
    wire_names: [&'static str; 5],
    image_id: &'static str,
    pose_clips: &'static [&'static str],
    startup_timeout_ms: u32,
    request_timeout_ms: u32,
}

#[derive(Serialize)]
struct Presentation {
    tile: u32,
    max_dimension: u32,
    floor: [&'static str; 2],
    grid: &'static str,
    cells: &'static [&'static [Paint]],
    player_cell: u8,
    status_messages: &'static [&'static str],
    failure_message: &'static str,
}

#[derive(Serialize)]
struct CanvasAdapter {
    wasm: Wasm,
    controls: Controls,
    assets: Assets,
    presentation: Presentation,
    debugger: Debugger,
}

#[derive(Serialize)]
struct Debugger {
    drawer: &'static str,
    pause: &'static str,
    step: &'static str,
    download: &'static str,
    replay_download: &'static str,
    overlay: &'static str,
    channels: &'static str,
    channel_attribute: &'static str,
    inspector: &'static str,
    trace: &'static str,
    timing: &'static str,
    graph: &'static str,
    feedback: &'static str,
    command: &'static str,
    command_input: &'static str,
    command_run: &'static str,
    channel_names: &'static [&'static str],
    phase_names: &'static [&'static str],
    event_names: &'static [&'static str],
    player_kind: u8,
    pause_label: &'static str,
    resume_label: &'static str,
    pause_key: &'static str,
    step_key: &'static str,
    pause_first: &'static str,
    command_help: &'static str,
    trace_filename: &'static str,
    replay_filename: &'static str,
    timing_filename: &'static str,
    trace_saved: &'static str,
    replay_saved: &'static str,
    timing_saved: &'static str,
    unavailable: &'static str,
    sample_capacity: usize,
    visible_trace_records: usize,
    budget_ms: f64,
    budget_color: &'static str,
    sample_color: &'static str,
    coordinate_color: &'static str,
    event_color: &'static str,
    scopes: TimingScopes,
}

#[derive(Serialize)]
struct TimingScopes {
    advance: &'static str,
    snapshot: &'static str,
    paint: &'static str,
    frame: &'static str,
}

use Paint::{FillCircle, FillRect, StrokeArc, StrokePath, StrokeRect};

const GOLD: &str = "#ffd272";
const LOCKED_FILL: &str = "#442e30";
const LOCKED_STROKE: &str = "#f5a38f";
const OPEN_STROKE: &str = "#79e1ad";

const PLAYTEST: CanvasAdapter = CanvasAdapter {
    wasm: Wasm {
        prefix: "forge_playtest_",
    },
    controls: Controls {
        board: "#playtest-board",
        title: "#playtest-title",
        status: "#playtest-status",
        level_status: "#playtest-load-status",
        level_group: "#playtest-levels",
        instructions: "#playtest-instructions",
        restart: "#playtest-restart",
        direction: "button[data-playtest-action]",
        action_attribute: "data-playtest-action",
        held_attribute: "data-playtest-held",
        action_count: 5,
        jump_action: 4,
        keys: &[
            ("arrowup", 0),
            ("w", 0),
            ("arrowdown", 1),
            ("s", 1),
            ("arrowleft", 2),
            ("a", 2),
            ("arrowright", 3),
            ("d", 3),
            (" ", 4),
        ],
    },
    assets: Assets {
        levels: &[
            LevelChoice {
                selector: "#playtest-room-one",
                label: "Room One",
                level: 1,
            },
            LevelChoice {
                selector: "#playtest-traversal",
                label: "Traversal Yard",
                level: 2,
            },
        ],
        worker_url: "/playtest/assets-worker.js",
        wire_names: [
            "manifest",
            "room-one",
            "traversal-yard",
            "scout-atlas",
            "scout-atlas-image",
        ],
        image_id: "scout-atlas-image",
        pose_clips: &["idle", "walk", "jump", "jump", "crouch"],
        startup_timeout_ms: 15_000,
        request_timeout_ms: 3500,
    },
    presentation: Presentation {
        tile: 56,
        max_dimension: 64,
        floor: ["#111d2c", "#152233"],
        grid: "#233349",
        cells: &[
            // Floor, wall, key, locked exit, open exit, player, guard.
            &[],
            &[
                FillRect([3, 3], [50, 50], "#25354b"),
                FillRect([3, 3], [50, 3], "#3b526d"),
                FillRect([3, 50], [50, 3], "#1b293b"),
            ],
            &[
                StrokeArc([20, 23], 7, [0.0, 1.0], GOLD, 5),
                StrokePath(&[[24, 28], [37, 41]], GOLD, 5),
                StrokePath(&[[30, 34], [35, 29]], GOLD, 5),
                StrokePath(&[[35, 39], [40, 34]], GOLD, 5),
            ],
            &[
                FillRect([11, 7], [34, 42], LOCKED_FILL),
                StrokeRect([11, 7], [34, 42], LOCKED_STROKE, 2),
                StrokeArc([28, 26], 5, [0.5, 1.0], LOCKED_STROKE, 2),
                FillRect([21, 25], [14, 11], LOCKED_STROKE),
                FillRect([27, 29], [2, 4], LOCKED_FILL),
            ],
            &[
                FillRect([11, 7], [34, 42], "#193d36"),
                StrokeRect([11, 7], [34, 42], OPEN_STROKE, 2),
                StrokePath(&[[19, 28], [36, 28]], OPEN_STROKE, 2),
                StrokePath(&[[29, 20], [37, 28], [29, 36]], OPEN_STROKE, 2),
            ],
            &[
                FillCircle([28, 28], 16, "#78afff"),
                FillRect([24, 24], [8, 8], "#162945"),
                FillRect([22, 17], [12, 2], "#e4f0ff"),
            ],
            &[
                FillRect([13, 14], [30, 28], "#443355"),
                StrokePath(
                    &[
                        [13, 28],
                        [20, 14],
                        [36, 14],
                        [43, 28],
                        [37, 42],
                        [19, 42],
                        [13, 28],
                    ],
                    "#e3abfa",
                    3,
                ),
                FillRect([19, 24], [18, 5], "#e3abfa"),
                FillRect([25, 25], [6, 4], "#443355"),
            ],
        ],
        player_cell: 5,
        status_messages: &["Find the key.", "Exit open. Reach the exit.", "You won!"],
        failure_message: "The playtest could not start. Reload to try again.",
    },
    debugger: Debugger {
        drawer: "#playtest-debug",
        pause: "#playtest-pause",
        step: "#playtest-step",
        download: "#playtest-export",
        replay_download: "#playtest-export-replay",
        overlay: "#playtest-overlay",
        channels: "[data-playtest-trace-channel]",
        channel_attribute: "data-playtest-trace-channel",
        inspector: "#playtest-inspector",
        trace: "#playtest-trace",
        timing: "#playtest-timing",
        graph: "#playtest-timing-graph",
        feedback: "#playtest-debug-status",
        command: "#playtest-command",
        command_input: "#playtest-command-input",
        command_run: "#playtest-command-run",
        channel_names: &["Input", "Simulation", "Events", "Level", "Hitch", "Error"],
        phase_names: &["Input", "Simulate", "Publish", "Present", "Drain"],
        event_names: &[
            "Event",
            "Key collected",
            "Exit unlocked",
            "Completed",
            "Blocked by guard",
        ],
        player_kind: 1,
        pause_label: "Pause",
        resume_label: "Resume",
        pause_key: "p",
        step_key: ".",
        pause_first: "Pause before stepping.",
        command_help:
            "Commands: pause, resume, step, restart, trace, replay, channels <mask>, timing.",
        trace_filename: "room-one-trace.json",
        replay_filename: "forge-playtest-replay.json",
        timing_filename: "room-one-timing.json",
        trace_saved: "Trace JSON downloaded.",
        replay_saved: "Session replay downloaded.",
        timing_saved: "Timing JSON downloaded.",
        unavailable: "Debugging needs the current playtest bundle.",
        sample_capacity: 120,
        visible_trace_records: 12,
        budget_ms: 1000.0 / 60.0,
        budget_color: "#e5b271",
        sample_color: "#78afff",
        coordinate_color: "#cfddf1",
        event_color: "#ffd272",
        scopes: TimingScopes {
            advance: "Advance",
            snapshot: "Snapshot",
            paint: "Paint",
            frame: "Frame CPU",
        },
    },
};

pub fn javascript() -> String {
    let config = serde_json::to_string(&PLAYTEST).expect("serialize Canvas adapter configuration");
    let readers = snapshot::SNAPSHOT_READERS;
    let debugger = debug::DEBUG_ADAPTER;
    let assets = assets::ASSET_ADAPTER;
    let replay = replay::REPLAY_ADAPTER;
    format!("'use strict';\n(config => {{\n  const {{ wasm, controls, assets: assetSpec, presentation, debugger: debugSpec }} = config;\n{readers}{debugger}{assets}{replay}{CANVAS_ADAPTER}\n}})({config});\n")
}

const CANVAS_ADAPTER: &str = r#"
  const { tile } = presentation;
  const board = document.querySelector(controls.board);
  const title = document.querySelector(controls.title);
  const status = document.querySelector(controls.status);
  const levelStatus = document.querySelector(controls.level_status);
  const levelGroup = document.querySelector(controls.level_group);
  const instructions = document.querySelector(controls.instructions);
  const restart = document.querySelector(controls.restart);
  const directions = [...document.querySelectorAll(controls.direction)];
  const choices = assetSpec.levels.map(choice => ({
    ...choice, button: document.querySelector(choice.selector),
  }));
  const keyActions = new Map(controls.keys);
  const turn = 2 * Math.PI;
  let failed = false;
  let io;

  function disableDebug(message) {
    const drawer = document.querySelector(debugSpec.drawer);
    for (const control of drawer?.querySelectorAll('button, input') ?? []) control.disabled = true;
    const feedback = document.querySelector(debugSpec.feedback);
    if (feedback) feedback.textContent = message;
  }

  function fail() {
    if (failed) return;
    failed = true;
    io?.close();
    if (status) status.textContent = presentation.failure_message;
    if (restart) restart.disabled = true;
    for (const { button } of choices) if (button) button.disabled = true;
    for (const button of directions) {
      button.disabled = true;
      button.removeAttribute(controls.held_attribute);
    }
    const ctx = board?.getContext('2d');
    if (ctx) ctx.clearRect(0, 0, board.width, board.height);
    for (const selector of [debugSpec.inspector, debugSpec.trace, debugSpec.timing]) {
      const output = document.querySelector(selector);
      if (output) output.textContent = '';
    }
    disableDebug(presentation.failure_message);
  }

  function guard(operation) {
    return (...args) => {
      if (failed) return;
      try {
        operation(...args);
      } catch (_) {
        fail();
      }
    };
  }

  function paintShape(ctx, { kind, args }) {
    switch (kind) {
      case 'fill_rect': {
        const [[x, y], [width, height], color] = args;
        ctx.fillStyle = color;
        ctx.fillRect(x, y, width, height);
        break;
      }
      case 'stroke_rect': {
        const [[x, y], [width, height], color, weight] = args;
        ctx.strokeStyle = color;
        ctx.lineWidth = weight;
        ctx.strokeRect(x, y, width, height);
        break;
      }
      case 'fill_circle': {
        const [[x, y], radius, color] = args;
        ctx.fillStyle = color;
        ctx.beginPath();
        ctx.arc(x, y, radius, 0, turn);
        ctx.fill();
        break;
      }
      case 'stroke_arc': {
        const [[x, y], radius, [start, end], color, weight] = args;
        ctx.strokeStyle = color;
        ctx.lineWidth = weight;
        ctx.beginPath();
        ctx.arc(x, y, radius, start * turn, end * turn);
        ctx.stroke();
        break;
      }
      case 'stroke_path': {
        const [[first, ...rest], color, weight] = args;
        ctx.strokeStyle = color;
        ctx.lineWidth = weight;
        ctx.beginPath();
        ctx.moveTo(...first);
        for (const point of rest) ctx.lineTo(...point);
        ctx.stroke();
        break;
      }
    }
  }

  function drawCell(ctx, column, row, cell) {
    ctx.save();
    ctx.translate(column * tile, row * tile);
    ctx.fillStyle = presentation.floor[(column + row) % presentation.floor.length];
    ctx.fillRect(0, 0, tile, tile);
    ctx.strokeStyle = presentation.grid;
    ctx.lineWidth = 1;
    ctx.strokeRect(0.5, 0.5, tile - 1, tile - 1);
    for (const shape of presentation.cells[cell] ?? []) paintShape(ctx, shape);
    ctx.restore();
  }

  function drawOverlay(ctx, width, height, events, contact, body, collision) {
    ctx.save();
    ctx.font = '10px monospace';
    ctx.fillStyle = debugSpec.coordinate_color;
    for (let row = 0; row < height; row++) {
      for (let column = 0; column < width; column++) {
        ctx.fillText(`${column},${height - 1 - row}`, column * tile + 4, row * tile + 13);
      }
    }
    ctx.strokeStyle = debugSpec.event_color;
    ctx.lineWidth = 2;
    for (const event of events) {
      const left = event.x * tile;
      const top = (height - 1 - event.y) * tile;
      ctx.strokeRect(left + 3, top + 3, tile - 6, tile - 6);
    }
    if (collision) {
      const q16 = value => value * tile / 65536;
      ctx.strokeStyle = debugSpec.coordinate_color;
      for (const [left, top, right, bottom] of collision.bodies) {
        ctx.strokeRect(q16(left), q16(top), q16(right - left), q16(bottom - top));
      }
      if (collision.ray) {
        const [x, y] = collision.ray.point.map(q16);
        ctx.strokeStyle = debugSpec.event_color;
        ctx.beginPath();
        ctx.moveTo(...collision.ray.origin.map(q16));
        ctx.lineTo(x, y);
        ctx.stroke();
        ctx.strokeRect(x - 3, y - 3, 6, 6);
      }
    } else if (body) {
      ctx.strokeStyle = debugSpec.coordinate_color;
      ctx.strokeRect(...body);
    }
    const contacts = collision?.contacts ?? (contact ? [{ ...contact, y: height - 1 - contact.y }] : []);
    for (const hit of contacts) {
      const normal = [[0, 0], [-1, 0], [1, 0], [0, -1], [0, 1]][hit.normal];
      if (!normal || hit.x < 0 || hit.x >= width || hit.y < 0 || hit.y >= height) continue;
      const [dx, dy] = normal;
      const x = (hit.x + 0.5) * tile, y = (hit.y + 0.5) * tile;
      ctx.strokeStyle = debugSpec.coordinate_color;
      ctx.beginPath();
      ctx.moveTo(x, y);
      ctx.lineTo(x + dx * tile * 0.38, y + dy * tile * 0.38);
      ctx.stroke();
      ctx.strokeRect(x - 2, y - 2, 4, 4);
      if (hit.depth > 0) ctx.fillText(`${hit.depth} Q16`, x + 4, y + 12);
    }
    ctx.restore();
  }

  function playback(exports) {
    const get = name => exports[wasm.prefix + name];
    const inputOps = ['input', 'advance', 'clear_input', 'restart'].map(get);
    if (inputOps.some(operation => typeof operation !== 'function')) throw new Error('Canvas WASM ABI mismatch');
    const [input, advance, resetInput, resetGame] = inputOps;
    const binary = ['snapshot', 'snapshot_len', 'trace', 'trace_len', 'trace_channels', 'step'].map(get);
    if (binary.every(operation => operation === undefined)) {
      const scalar = ['width', 'height', 'cell', 'status'].map(get);
      if (scalar.some(operation => typeof operation !== 'function')) throw new Error('Canvas WASM ABI mismatch');
      const [getWidth, getHeight, getCell, getStatus] = scalar;
      const width = getWidth(), height = getHeight();
      if (width < 1 || height < 1 || width > presentation.max_dimension || height > presentation.max_dimension) {
        throw new Error('Playtest board dimensions out of range');
      }
      const cells = new Uint8Array(width * height);
      return { input, advance, resetInput, resetGame, read: () => {
        for (let row = 0; row < height; row++) {
          for (let column = 0; column < width; column++) cells[row * width + column] = getCell(column, row);
        }
        return { width, height, cells, status: getStatus() };
      }};
    }
    if (binary.some(operation => typeof operation !== 'function') || !(exports.memory instanceof WebAssembly.Memory)) {
      throw new Error('Canvas WASM snapshot ABI mismatch');
    }
    const [snapshotOffset, snapshotLength, traceOffset, traceLength, traceChannels, step] = binary;
    const snapshotView = memoryView(exports.memory, snapshotOffset, snapshotLength, snapshotBytes, 'Snapshot');
    const traceView = memoryView(exports.memory, traceOffset, traceLength, traceBytes, 'Trace');
    const poseOffset = get('pose_snapshot'), poseLength = get('pose_snapshot_len');
    const poseView = typeof poseOffset === 'function' && typeof poseLength === 'function'
      ? memoryView(exports.memory, poseOffset, poseLength, poseBytes, 'Pose') : null;
    const activationOffset = get('level_snapshot'), activationLength = get('level_snapshot_len');
    const activationView = typeof activationOffset === 'function' && typeof activationLength === 'function'
      ? memoryView(exports.memory, activationOffset, activationLength, activationBytes, 'Activation') : null;
    const replayOffset = get('replay'), replayLength = get('replay_len');
    const activeLevel = get('level');
    const setPaused = get('set_paused');
    const collisionOffset = get('collision_snapshot'), collisionLength = get('collision_snapshot_len');
    const collisionView = typeof collisionOffset === 'function' && typeof collisionLength === 'function'
      ? memoryView(exports.memory, collisionOffset, collisionLength, collisionBytes, 'Collision') : null;
    return {
      input, advance, resetInput, resetGame, step, traceChannels,
      read: () => readSnapshot(snapshotView()),
      trace: epoch => readTrace(traceView(), epoch),
      assets: assetAdmission(get, exports.memory),
      activeLevel: typeof activeLevel === 'function' ? activeLevel : null,
      setPaused: typeof setPaused === 'function' ? setPaused : null,
      pose: poseView && (snapshot => readPose(poseView(), snapshot)),
      activation: activationView && (snapshot => readActivation(activationView(), snapshot)),
      collision: collisionView && (snapshot => readCollision(collisionView(), snapshot)),
      replayPlayback: replayControls(get, exports.memory),
      replay: typeof replayOffset === 'function' && typeof replayLength === 'function' ? () => {
        const offset = replayOffset() >>> 0;
        const length = replayLength() >>> 0;
        const memory = exports.memory.buffer;
        if (!offset || !length || length > 2 * 1024 * 1024 || offset + length > memory.byteLength) {
          throw new Error('Invalid replay export');
        }
        return new Uint8Array(memory, offset, length).slice();
      } : null,
      memoryBytes: () => exports.memory.buffer.byteLength,
    };
  }

  async function start() {
    const ctx = board?.getContext('2d', { alpha: false });
    if (!ctx || !status || !restart) throw new Error('Canvas controls unavailable');
    io = workerAssetIo(assetSpec.worker_url);
    const startup = new AbortController();
    const timeout = setTimeout(() => startup.abort(), assetSpec.startup_timeout_ms);
    const bytes = await io.read('wasm', startup.signal).finally(() => clearTimeout(timeout));
    const { instance } = await WebAssembly.instantiate(bytes, {});
    if (failed) return;
    const runtime = playback(instance.exports);
    const { input, advance, resetInput, resetGame } = runtime;

    let previousFrame = null;
    let visibleStatus = null;
    let focused = document.hasFocus();
    let debuggerPanel;
    let paintedCells;
    let priorDrawing;
    let wasOverlay = false;
    let atlas;
    let selectedLevel;
    let pending;
    let activeCollider;
    let handoff = null;
    let audio;
    const loader = runtime.assets && runtime.activeLevel && levelGroup && levelStatus && choices.every(choice => choice.button)
      ? cookedAssets(runtime.assets, assetSpec, io) : null;
    const buttons = directions.map(button => ({
      button, action: Number(button.getAttribute(controls.action_attribute)),
    }));
    const keys = new Map();
    const pointers = new Map();
    const held = new Uint16Array(controls.action_count);

    function traversal() {
      return selectedLevel === assetSpec.levels[1].level;
    }

    function canUse(action) {
      return traversal() ? action >= 2 : action !== controls.jump_action;
    }

    function setHeld(action, change) {
      const wasHeld = held[action] !== 0;
      held[action] += change;
      const isHeld = held[action] !== 0;
      if (isHeld === wasHeld) return;
      input(action, Number(isHeld));
      for (const control of buttons) {
        if (control.action === action) {
          control.button.toggleAttribute(controls.held_attribute, isHeld);
        }
      }
      if (debuggerPanel?.paused()) refresh();
    }

    function clearDeviceInput() {
      previousFrame = null;
      keys.clear();
      pointers.clear();
      held.fill(0);
      for (const { button } of buttons) button.removeAttribute(controls.held_attribute);
    }

    function clearInput() {
      clearDeviceInput();
      resetInput();
    }

    function setPaused(paused) {
      clearDeviceInput();
      if (runtime.setPaused) runtime.setPaused(Number(paused));
      else resetInput();
    }

    function levelMessage(message) {
      if (!levelStatus) return;
      levelStatus.textContent = message;
      levelStatus.hidden = !message;
    }

    function armEntryCue() {
      try {
        audio ??= new AudioContext();
        if (audio.state === 'suspended') void audio.resume().catch(() => {});
      } catch (_) {
        // The text cue remains available on browsers without audio support.
      }
    }

    function entryCue() {
      if (audio?.state !== 'running') return;
      try {
        const tone = audio.createOscillator();
        const gain = audio.createGain();
        const start = audio.currentTime;
        tone.frequency.value = 660;
        gain.gain.setValueAtTime(0.04, start);
        gain.gain.exponentialRampToValueAtTime(0.001, start + 0.08);
        tone.connect(gain).connect(audio.destination);
        tone.start(start);
        tone.stop(start + 0.08);
      } catch (_) {
        // A device change can invalidate the audio route during activation.
      }
    }

    function syncChoices() {
      if (!loader) return;
      levelGroup.setAttribute('aria-busy', String(Boolean(pending)));
      for (const choice of choices) {
        choice.button.disabled = Boolean(pending);
        choice.button.setAttribute('aria-pressed', String(
          selectedLevel === choice.level || (selectedLevel === 0 && choice.level === choices[0].level)
        ));
      }
    }

    function syncLevel() {
      const active = runtime.activeLevel?.() ?? 0;
      if (active !== selectedLevel) {
        if (selectedLevel !== undefined) clearInput();
        selectedLevel = active;
        activeCollider = active && runtime.assets ? runtime.assets.collider() : null;
        const label = choices.find(choice => choice.level === active)?.label ?? choices[0].label;
        if (title) title.textContent = label;
        board.setAttribute('aria-label', `${label} game board`);
        if (instructions) {
          instructions.textContent = traversal()
            ? 'Move with Left and Right or A and D. Press Space or use Jump.'
            : 'Use arrow keys or WASD, or hold a direction below.';
        }
        for (const { button, action } of buttons) {
          if (Number.isInteger(action) && action >= 0 && action < held.length) {
            button.disabled = !canUse(action);
            button.hidden = button.disabled;
          }
        }
        syncChoices();
      }
      if (pending?.staged && pending.choice.level === active) {
        const focusBoard = document.activeElement === pending.choice.button;
        const { choice, user, startBytes, stagedBytes } = pending;
        if (runtime.memoryBytes) {
          handoff = { level: choice.label, startBytes, peakBytes: Math.max(startBytes, stagedBytes, runtime.memoryBytes()) };
        }
        pending = null;
        levelMessage(user ? `Entered ${choice.label}.` : '');
        if (user) entryCue();
        syncChoices();
        if (focusBoard) board.focus({ preventScroll: true });
      }
    }

    async function requestLevel(choice, user = false) {
      if (!loader || pending || runtime.activeLevel() === choice.level) return;
      if (user) armEntryCue();
      const controller = new AbortController();
      const load = { choice, controller, user, staged: false, startBytes: runtime.memoryBytes?.() };
      pending = load;
      syncChoices();
      levelMessage(`Loading ${choice.label}…`);
      const timeout = setTimeout(() => controller.abort(), assetSpec.request_timeout_ms);
      try {
        await loader.loadLevel(choice, controller.signal);
        if (failed || pending !== load) return;
        load.staged = true;
        load.stagedBytes = runtime.memoryBytes?.();
        if (debuggerPanel?.paused()) levelMessage(`${choice.label} is ready. Resume or step to open it.`);
      } catch (_) {
        if (pending === load && !failed) {
          pending = null;
          syncChoices();
          levelMessage(`Could not open ${choice.label}. The current level is still available.`);
        }
      } finally {
        clearTimeout(timeout);
      }
    }

    function drawing(snapshot, pose) {
      const rect = atlas?.clips.get(pose?.clip)?.[pose.frame];
      if (!pose || (!rect && !traversal())) return null;
      const weight = debuggerPanel?.paused() ? 1 : snapshot.fraction / 1000000;
      const mix = (previous, current) => previous + (current - previous) * weight;
      const player = pose.player;
      const x = (mix(player.previousX, player.x) + 0.5) * tile;
      const y = (snapshot.height - mix(player.previousY, player.y)) * tile;
      if (!rect) {
        return { x, y, bounds: [x - tile / 2, y - tile, x + tile / 2, y + tile] };
      }
      const scale = tile / Math.max(atlas.size[0], atlas.size[1] * 0.72);
      const [left, top] = atlas.pivot.map(value => -value * scale);
      const [width, height] = atlas.size.map(value => value * scale);
      const visualLeft = pose.facing ? x - left - width : x + left;
      return {
        x, y, rect, left, top, width, height, flip: pose.facing === 1,
        bounds: [visualLeft, y + top, visualLeft + width, y + top + height],
      };
    }

    function drawPlayer(player) {
      ctx.save();
      ctx.translate(player.x, player.y);
      if (player.rect) {
        if (player.flip) ctx.scale(-1, 1);
        ctx.imageSmoothingEnabled = true;
        ctx.drawImage(atlas.image, ...player.rect, player.left, player.top, player.width, player.height);
      } else {
        ctx.translate(-tile / 2, -44);
        for (const shape of presentation.cells[presentation.player_cell]) paintShape(ctx, shape);
      }
      ctx.restore();
    }

    function paint({ width, height, cells, status: currentStatus }, overlay, events, player, contact, collision) {
      if (currentStatus < 0 || currentStatus >= presentation.status_messages.length ||
          cells.some(cell => cell >= presentation.cells.length)) throw new Error('Invalid playtest cell or status');
      if (!paintedCells || board.width !== width * tile || board.height !== height * tile) {
        board.width = width * tile;
        board.height = height * tile;
        ctx.imageSmoothingEnabled = false;
        paintedCells = new Int16Array(width * height).fill(-1);
      }
      const last = priorDrawing?.bounds;
      for (let row = 0; row < height; row++) {
        for (let column = 0; column < width; column++) {
          const position = row * width + column;
          const cell = player && cells[position] === presentation.player_cell ? 0 : cells[position];
          const underLast = last && column * tile < last[2] && (column + 1) * tile > last[0] &&
            row * tile < last[3] && (row + 1) * tile > last[1];
          if (overlay || wasOverlay || underLast || paintedCells[position] !== cell) {
            drawCell(ctx, column, row, cell);
            paintedCells[position] = cell;
          }
        }
      }
      if (player) drawPlayer(player);
      const art = player?.rect ? 'atlas' : 'primitive';
      if (board.dataset.playtestArt !== art) board.dataset.playtestArt = art;
      priorDrawing = player;
      if (overlay) {
        let body;
        if (player && traversal() && activeCollider) {
          const [left, top, right, bottom] = activeCollider.map(edge => edge / 65536 * tile);
          body = [player.x + left, player.y + top, right - left, bottom - top];
        }
        drawOverlay(ctx, width, height, events, contact, body, collision);
      }
      wasOverlay = overlay;
      if (currentStatus !== visibleStatus) {
        status.textContent = presentation.status_messages[currentStatus];
        visibleStatus = currentStatus;
      }
    }

    function showBoard(snapshot) {
      const overlay = Boolean(debuggerPanel?.overlay());
      const events = overlay ? snapshotEvents(snapshot).slice(-12) : [];
      const pose = runtime.pose?.(snapshot);
      paint(snapshot, overlay, events, drawing(snapshot, pose), pose?.contact,
        overlay ? runtime.collision?.(snapshot) : null);
    }

    const refresh = guard(() => {
      syncLevel();
      const snapshot = runtime.read();
      showBoard(snapshot);
      debuggerPanel?.present(snapshot, true);
    });

    const restartRoom = guard(() => {
      if (pending) {
        pending.controller.abort();
        pending = null;
        levelMessage('');
        syncChoices();
      }
      clearInput();
      resetGame();
      refresh();
    });

    const frame = guard(time => {
      if (!debuggerPanel?.paused() && focused && !document.hidden) {
        const start = performance.now();
        if (previousFrame !== null) {
          const micros = Math.round((time - previousFrame) * 1000);
          advance(Math.max(0, Math.min(micros, 0xffffffff)));
        }
        syncLevel();
        previousFrame = time;
        const simulated = performance.now();
        const snapshot = runtime.read();
        const read = performance.now();
        showBoard(snapshot);
        const present = performance.now();
        debuggerPanel?.record({
          advance: simulated - start, snapshot: read - simulated,
          paint: present - read, frame: present - start,
        });
        debuggerPanel?.present(snapshot, false);
      }
      requestAnimationFrame(frame);
    });

    if (runtime.trace) {
      debuggerPanel = attachDebugger(debugSpec, {
        ...runtime, snapshot: runtime.read, handoff: () => handoff, setPaused, refresh, restart: restartRoom, guard,
      });
      attachReplay(runtime.replayPlayback, io, guard);
    } else {
      disableDebug(debugSpec.unavailable);
    }

    window.addEventListener('keydown', guard(event => {
      const key = event.key.toLowerCase();
      const action = keyActions.get(key);
      if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
      if (event.target instanceof Element && event.target.closest('input, select, textarea, [contenteditable], #playtest-replay')) return;
      if (document.hidden || !focused) return;
      if (debuggerPanel?.shortcut(event, key)) {
        event.preventDefault();
        return;
      }
      if (action === undefined || !canUse(action)) return;
      if (action === controls.jump_action && event.target instanceof Element &&
          event.target.closest('button, a')) return;
      event.preventDefault();
      const id = event.code || key;
      if (event.repeat || keys.has(id)) return;
      keys.set(id, action);
      setHeld(action, 1);
    }));
    window.addEventListener('keyup', guard(event => {
      const id = event.code || event.key.toLowerCase();
      const action = keys.get(id);
      if (action === undefined) return;
      event.preventDefault();
      keys.delete(id);
      setHeld(action, -1);
    }));

    function endPointer(event) {
      const action = pointers.get(event.pointerId);
      if (action === undefined) return;
      pointers.delete(event.pointerId);
      setHeld(action, -1);
    }

    for (const { button, action } of buttons) {
      if (!Number.isInteger(action) || action < 0 || action >= held.length) continue;
      button.addEventListener('pointerdown', guard(event => {
        if (event.button !== 0 || pointers.has(event.pointerId) || !canUse(action)) return;
        event.preventDefault();
        button.focus({ preventScroll: true });
        pointers.set(event.pointerId, action);
        setHeld(action, 1);
        try {
          button.setPointerCapture(event.pointerId);
        } catch (_) {
          // Window listeners still release a pointer without capture.
        }
      }));
      button.addEventListener('lostpointercapture', guard(endPointer));
      button.addEventListener('click', guard(event => {
        // Pointer presses already provide down/up. Keyboard and assistive
        // activation arrive as a click without a pointer click count.
        if (event.detail !== 0 || !canUse(action)) return;
        setHeld(action, 1);
        setHeld(action, -1);
      }));
    }
    window.addEventListener('pointerup', guard(endPointer));
    window.addEventListener('pointercancel', guard(endPointer));
    window.addEventListener('blur', guard(() => {
      focused = false;
      clearInput();
    }));
    window.addEventListener('focus', guard(() => {
      focused = true;
      clearInput();
    }));
    document.addEventListener('visibilitychange', guard(clearInput));
    window.addEventListener('pagehide', guard(event => {
      clearInput();
      if (!event.persisted) io.close();
      if (!event.persisted && audio) void audio.close().catch(() => {});
    }));
    window.addEventListener('pageshow', guard(clearInput));
    if (loader) {
      levelGroup.hidden = false;
      for (const choice of choices) {
        choice.button.addEventListener('click', guard(() => { void requestLevel(choice, true); }));
      }
      await requestLevel(choices[0]);
      if (runtime.pose) {
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), assetSpec.request_timeout_ms * 2);
        loader.loadAtlas(controller.signal).then(value => {
          if (failed) {
            value.image.close();
            return;
          }
          atlas = value;
          refresh();
        }).catch(() => {}).finally(() => clearTimeout(timeout));
      }
    }
    restart.addEventListener('click', restartRoom);
    restart.disabled = false;
    refresh();
    if (!failed) requestAnimationFrame(frame);
  }

  start().catch(fail);
"#;
