//! Build-time emitter for the Canvas/WebAssembly adapter.
//!
//! Rust supplies the page bindings and tile visuals. The browser adapter owns
//! device capture, presentation, and its wall clock. WASM owns game state.

use serde::Serialize;

#[path = "playtest_debug_browser.rs"]
mod debug;
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
    path: &'static str,
    prefix: &'static str,
}

#[derive(Serialize)]
struct Controls {
    board: &'static str,
    status: &'static str,
    restart: &'static str,
    direction: &'static str,
    action_attribute: &'static str,
    held_attribute: &'static str,
    action_count: usize,
    keys: &'static [(&'static str, u8)],
}

#[derive(Serialize)]
struct Presentation {
    tile: u32,
    max_dimension: u32,
    floor: [&'static str; 2],
    grid: &'static str,
    cells: &'static [&'static [Paint]],
    status_messages: &'static [&'static str],
    failure_message: &'static str,
}

#[derive(Serialize)]
struct CanvasAdapter {
    wasm: Wasm,
    controls: Controls,
    presentation: Presentation,
    debugger: Debugger,
}

#[derive(Serialize)]
struct Debugger {
    drawer: &'static str,
    pause: &'static str,
    step: &'static str,
    download: &'static str,
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
    timing_filename: &'static str,
    trace_saved: &'static str,
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
        path: "/app.wasm",
        prefix: "forge_playtest_",
    },
    controls: Controls {
        board: "#playtest-board",
        status: "#playtest-status",
        restart: "#playtest-restart",
        direction: "button[data-playtest-action]",
        action_attribute: "data-playtest-action",
        held_attribute: "data-playtest-held",
        action_count: 4,
        keys: &[
            ("arrowup", 0),
            ("w", 0),
            ("arrowdown", 1),
            ("s", 1),
            ("arrowleft", 2),
            ("a", 2),
            ("arrowright", 3),
            ("d", 3),
        ],
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
        status_messages: &["Find the key.", "Exit open. Reach the exit.", "You won!"],
        failure_message: "The playtest could not start. Reload to try again.",
    },
    debugger: Debugger {
        drawer: "#playtest-debug",
        pause: "#playtest-pause",
        step: "#playtest-step",
        download: "#playtest-export",
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
        command_help: "Commands: pause, resume, step, restart, trace, channels <mask>, timing.",
        trace_filename: "room-one-trace.json",
        timing_filename: "room-one-timing.json",
        trace_saved: "Trace JSON downloaded.",
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
    format!("'use strict';\n(config => {{\n  const {{ wasm, controls, presentation, debugger: debugSpec }} = config;\n{readers}{debugger}{CANVAS_ADAPTER}\n}})({config});\n")
}

const CANVAS_ADAPTER: &str = r#"
  const { tile } = presentation;
  const board = document.querySelector(controls.board);
  const status = document.querySelector(controls.status);
  const restart = document.querySelector(controls.restart);
  const directions = [...document.querySelectorAll(controls.direction)];
  const keyActions = new Map(controls.keys);
  const turn = 2 * Math.PI;
  let failed = false;

  function disableDebug(message) {
    const drawer = document.querySelector(debugSpec.drawer);
    for (const control of drawer?.querySelectorAll('button, input') ?? []) control.disabled = true;
    const feedback = document.querySelector(debugSpec.feedback);
    if (feedback) feedback.textContent = message;
  }

  function fail() {
    if (failed) return;
    failed = true;
    if (status) status.textContent = presentation.failure_message;
    if (restart) restart.disabled = true;
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

  function drawOverlay(ctx, width, height, events) {
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
    return {
      input, advance, resetInput, resetGame, step, traceChannels,
      read: () => readSnapshot(snapshotView()),
      trace: epoch => readTrace(traceView(), epoch),
      memoryBytes: () => exports.memory.buffer.byteLength,
    };
  }

  async function start() {
    const ctx = board?.getContext('2d', { alpha: false });
    if (!ctx || !status || !restart) throw new Error('Canvas controls unavailable');
    const response = await fetch(wasm.path, {
      mode: 'same-origin', credentials: 'same-origin', redirect: 'error',
    });
    if (!response.ok) throw new Error('Canvas WASM asset unavailable');
    const { instance } = await WebAssembly.instantiateStreaming(response, {});
    const runtime = playback(instance.exports);
    const { input, advance, resetInput, resetGame } = runtime;

    let previousFrame = null;
    let visibleStatus = null;
    let focused = document.hasFocus();
    let debuggerPanel;
    let paintedCells;
    let wasOverlay = false;
    const buttons = directions.map(button => ({
      button, action: Number(button.getAttribute(controls.action_attribute)),
    }));
    const keys = new Map();
    const pointers = new Map();
    const held = new Uint16Array(controls.action_count);

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

    function clearInput() {
      previousFrame = null;
      keys.clear();
      pointers.clear();
      held.fill(0);
      resetInput();
      for (const { button } of buttons) button.removeAttribute(controls.held_attribute);
    }

    function paint({ width, height, cells, status: currentStatus }, overlay, events) {
      if (currentStatus < 0 || currentStatus >= presentation.status_messages.length ||
          cells.some(cell => cell >= presentation.cells.length)) throw new Error('Invalid playtest cell or status');
      if (!paintedCells || board.width !== width * tile || board.height !== height * tile) {
        board.width = width * tile;
        board.height = height * tile;
        ctx.imageSmoothingEnabled = false;
        paintedCells = new Int16Array(width * height).fill(-1);
      }
      for (let row = 0; row < height; row++) {
        for (let column = 0; column < width; column++) {
          const position = row * width + column;
          const cell = cells[position];
          if (overlay || wasOverlay || paintedCells[position] !== cell) {
            drawCell(ctx, column, row, cell);
            paintedCells[position] = cell;
          }
        }
      }
      if (overlay) drawOverlay(ctx, width, height, events);
      wasOverlay = overlay;
      if (currentStatus !== visibleStatus) {
        status.textContent = presentation.status_messages[currentStatus];
        visibleStatus = currentStatus;
      }
    }

    function showBoard(snapshot) {
      const overlay = Boolean(debuggerPanel?.overlay());
      const events = overlay ? snapshotEvents(snapshot).slice(-12) : [];
      paint(snapshot, overlay, events);
    }

    const refresh = guard(() => {
      const snapshot = runtime.read();
      showBoard(snapshot);
      debuggerPanel?.present(snapshot, true);
    });

    const restartRoom = guard(() => {
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
        ...runtime, snapshot: runtime.read, clearInput, refresh, restart: restartRoom, guard,
      });
    } else {
      disableDebug(debugSpec.unavailable);
    }

    window.addEventListener('keydown', guard(event => {
      const key = event.key.toLowerCase();
      const action = keyActions.get(key);
      if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
      if (event.target instanceof Element && event.target.closest('input, select, textarea, [contenteditable]')) return;
      if (document.hidden || !focused) return;
      if (debuggerPanel?.shortcut(event, key)) {
        event.preventDefault();
        return;
      }
      if (action === undefined) return;
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
        if (event.button !== 0 || pointers.has(event.pointerId)) return;
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
        if (event.detail !== 0) return;
        setHeld(action, 1);
        setHeld(action, -1);
      }));
      button.disabled = false;
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
    window.addEventListener('pagehide', guard(clearInput));
    window.addEventListener('pageshow', guard(clearInput));
    restart.addEventListener('click', restartRoom);
    restart.disabled = false;
    refresh();
    if (!failed) requestAnimationFrame(frame);
  }

  start().catch(fail);
"#;
