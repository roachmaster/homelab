//! Build-time emitter for the scalar Canvas/WebAssembly adapter.
//!
//! Rust supplies the page bindings and tile visuals. The browser adapter owns
//! device capture, presentation, and its wall clock. WASM owns game state.

use serde::Serialize;

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
            // Floor, wall, key, locked exit, open exit, player.
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
        ],
        status_messages: &["Find the key.", "Exit open. Reach the exit.", "You won!"],
        failure_message: "The playtest could not start. Reload to try again.",
    },
};

pub fn javascript() -> String {
    let config = serde_json::to_string(&PLAYTEST).expect("serialize Canvas adapter configuration");
    format!("'use strict';\n{CANVAS_ADAPTER}({config});\n")
}

const CANVAS_ADAPTER: &str = r#"(config => {
  const { wasm, controls, presentation } = config;
  const { tile } = presentation;
  const board = document.querySelector(controls.board);
  const status = document.querySelector(controls.status);
  const restart = document.querySelector(controls.restart);
  const directions = [...document.querySelectorAll(controls.direction)];
  const keyActions = new Map(controls.keys);
  const turn = 2 * Math.PI;

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

  async function start() {
    const ctx = board?.getContext('2d', { alpha: false });
    if (!ctx || !status || !restart) throw new Error('Canvas controls unavailable');
    const response = await fetch(wasm.path, {
      mode: 'same-origin', credentials: 'omit', redirect: 'error',
    });
    if (!response.ok) throw new Error('Canvas WASM asset unavailable');
    const { instance } = await WebAssembly.instantiateStreaming(response, {});
    const exports = [
      'width', 'height', 'cell', 'status', 'input', 'advance', 'clear_input', 'restart',
    ].map(name => instance.exports[wasm.prefix + name]);
    if (exports.some(operation => typeof operation !== 'function')) {
      throw new Error('Canvas WASM ABI mismatch');
    }
    const [width, height, readCell, readStatus, input, advance, resetInput, resetGame] = exports;
    const columns = width();
    const rows = height();
    if (columns < 1 || columns > presentation.max_dimension || rows < 1 || rows > presentation.max_dimension) {
      throw new Error('Canvas board dimensions out of range');
    }
    board.width = columns * tile;
    board.height = rows * tile;
    ctx.imageSmoothingEnabled = false;

    let previousFrame = null;
    let visibleStatus = null;
    let focused = document.hasFocus();
    const buttons = directions.map(button => ({
      button, action: Number(button.getAttribute(controls.action_attribute)),
    }));
    const keys = new Map();
    const pointers = new Map();
    const held = new Uint16Array(controls.action_count);
    const paintedCells = new Int32Array(columns * rows).fill(-1);

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
    }

    function clearInput() {
      previousFrame = null;
      keys.clear();
      pointers.clear();
      held.fill(0);
      resetInput();
      for (const { button } of buttons) button.removeAttribute(controls.held_attribute);
    }

    function paint() {
      for (let row = 0; row < rows; row++) {
        for (let column = 0; column < columns; column++) {
          const position = row * columns + column;
          const cell = readCell(column, row);
          if (paintedCells[position] !== cell) {
            drawCell(ctx, column, row, cell);
            paintedCells[position] = cell;
          }
        }
      }
      const currentStatus = readStatus();
      if (currentStatus !== visibleStatus) {
        status.textContent = presentation.status_messages[currentStatus] ?? '';
        visibleStatus = currentStatus;
      }
    }

    function frame(time) {
      if (focused && !document.hidden) {
        if (previousFrame !== null) {
          const micros = Math.round((time - previousFrame) * 1000);
          advance(Math.max(0, Math.min(micros, 0xffffffff)));
        }
        previousFrame = time;
        paint();
      }
      requestAnimationFrame(frame);
    }

    window.addEventListener('keydown', event => {
      const key = event.key.toLowerCase();
      const action = keyActions.get(key);
      if (action === undefined || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
      if (event.target instanceof Element && event.target.closest('input, select, textarea, [contenteditable]')) return;
      if (document.hidden || !focused) return;
      event.preventDefault();
      const id = event.code || key;
      if (event.repeat || keys.has(id)) return;
      keys.set(id, action);
      setHeld(action, 1);
    });
    window.addEventListener('keyup', event => {
      const id = event.code || event.key.toLowerCase();
      const action = keys.get(id);
      if (action === undefined) return;
      event.preventDefault();
      keys.delete(id);
      setHeld(action, -1);
    });

    function endPointer(event) {
      const action = pointers.get(event.pointerId);
      if (action === undefined) return;
      pointers.delete(event.pointerId);
      setHeld(action, -1);
    }

    for (const { button, action } of buttons) {
      if (!Number.isInteger(action) || action < 0 || action >= held.length) continue;
      button.addEventListener('pointerdown', event => {
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
      });
      button.addEventListener('lostpointercapture', endPointer);
      button.addEventListener('click', event => {
        // Pointer presses already provide down/up. Keyboard and assistive
        // activation arrive as a click without a pointer click count.
        if (event.detail !== 0) return;
        setHeld(action, 1);
        setHeld(action, -1);
      });
      button.disabled = false;
    }
    window.addEventListener('pointerup', endPointer);
    window.addEventListener('pointercancel', endPointer);
    window.addEventListener('blur', () => {
      focused = false;
      clearInput();
    });
    window.addEventListener('focus', () => {
      focused = true;
      clearInput();
    });
    document.addEventListener('visibilitychange', clearInput);
    window.addEventListener('pagehide', clearInput);
    window.addEventListener('pageshow', clearInput);
    restart.addEventListener('click', () => {
      clearInput();
      resetGame();
      paint();
    });
    restart.disabled = false;
    paint();
    requestAnimationFrame(frame);
  }

  start().catch(() => {
    if (status) status.textContent = presentation.failure_message;
    if (restart) restart.disabled = true;
    for (const button of directions) button.disabled = true;
  });
})"#;
