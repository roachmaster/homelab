//! Browser controls for the Rust-owned replay timeline.

pub(super) const REPLAY_ADAPTER: &str = r#"
  function replayControls(get, memory) {
    const operations = [
      'replay_reserve', 'replay_prepare', 'replay_archive_read', 'replay_archive_cancel',
      'asset_reserve', 'replay_archive_accept', 'replay_finish',
      'replay_snapshot', 'replay_pose', 'replay_restart', 'replay_step', 'replay_done',
    ].map(get);
    if (operations.some(operation => typeof operation !== 'function')) return null;
    const [reserve, prepare, read, cancel, reserveAsset, accept, finish,
      snapshotOffset, poseOffset, restart, step, done] = operations;
    const snapshotView = memoryView(memory, snapshotOffset, () => snapshotBytes, snapshotBytes, 'Replay');
    const poseView = memoryView(memory, poseOffset, () => poseBytes, poseBytes, 'Replay pose');

    function copy(offset, bytes) {
      const at = offset >>> 0;
      if (!at || bytes.byteLength > memory.buffer.byteLength - at) return false;
      new Uint8Array(memory.buffer, at, bytes.byteLength).set(new Uint8Array(bytes));
      return true;
    }

    return {
      prepare(bytes) {
        return copy(reserve(bytes.byteLength), bytes) && prepare() === 1;
      },
      read() {
        const offset = read() >>> 0;
        if (!offset) return null;
        const view = new DataView(memory.buffer, offset, 44);
        if (view.getUint32(0, true) !== 0x31524746) throw new Error('Unknown replay read schema');
        const generation = view.getUint32(4, true);
        const asset = view.getUint32(8, true);
        const digest = Array.from(new Uint8Array(memory.buffer, offset + 12, 32),
          byte => byte.toString(16).padStart(2, '0')).join('');
        return { generation, asset, digest };
      },
      accept({ generation, asset }, bytes) {
        return copy(reserveAsset(asset, bytes.byteLength), bytes) && accept(generation) === 1;
      },
      cancel, finish, restart, step, done,
      frame() {
        const snapshot = readSnapshot(snapshotView());
        return [snapshot, readPose(poseView(), snapshot)];
      },
    };
  }

  function attachReplay(replay, io, guard) {
    if (!replay) return;
    const panel = document.getElementById('playtest-replay');
    const file = document.getElementById('playtest-replay-file');
    const notice = document.getElementById('playtest-replay-status');
    const controls = document.getElementById('playtest-replay-actions');
    const play = document.getElementById('playtest-replay-play');
    const step = document.getElementById('playtest-replay-step');
    const reset = document.getElementById('playtest-replay-reset');
    const position = document.getElementById('playtest-replay-position');
    const canvas = document.getElementById('playtest-replay-board');
    const ctx = canvas?.getContext('2d', { alpha: false });
    if (!panel || !file || !notice || !controls || !play || !step || !reset || !position || !ctx) return;
    const journalLimit = 2 * 1024 * 1024;
    const tickMs = 1000 / 60;
    let running = false;
    let scheduled = 0;
    let previous = 0;
    let elapsed = 0;

    function draw() {
      const [snapshot, pose] = replay.frame();
      const { width, height, cells, epoch, tick } = snapshot;
      if (canvas.width !== width * tile || canvas.height !== height * tile) {
        canvas.width = width * tile;
        canvas.height = height * tile;
      }
      for (let row = 0; row < height; row++) {
        for (let col = 0; col < width; col++) {
          const cell = cells[row * width + col];
          drawCell(ctx, col, row, pose && cell === presentation.player_cell ? 0 : cell);
        }
      }
      if (pose) {
        ctx.save();
        ctx.translate(pose.player.x * tile, (height - pose.player.y) * tile - 44);
        for (const shape of presentation.cells[presentation.player_cell]) paintShape(ctx, shape);
        ctx.restore();
      }
      position.textContent = `Segment ${epoch} · Tick ${tick}`;
      play.disabled = replay.done() === 1;
      step.disabled = running || replay.done() === 1;
      play.textContent = running ? 'Pause replay' : 'Play replay';
    }

    function stop() {
      running = false;
      cancelAnimationFrame(scheduled);
      elapsed = 0;
      previous = 0;
    }

    const frame = guard(time => {
      if (!running) return;
      if (previous) elapsed += Math.min(time - previous, tickMs * 4);
      previous = time;
      const ticks = Math.min(Math.floor(elapsed / tickMs), 4);
      if (ticks > 0) {
        replay.step(ticks);
        elapsed -= ticks * tickMs;
        if (replay.done() === 1) {
          stop();
          notice.textContent = 'Replay complete.';
        }
        draw();
      }
      if (running) scheduled = requestAnimationFrame(frame);
    });

    async function loadArchive() {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), assetSpec.startup_timeout_ms);
      try {
        for (let request = replay.read(); request; request = replay.read()) {
          const bytes = await io.read(assetSpec.wire_names[request.asset], controller.signal, request.digest);
          controller.signal.throwIfAborted();
          if (!replay.accept(request, bytes)) {
            throw new Error('Replay archive verification failed');
          }
        }
      } finally {
        clearTimeout(timer);
      }
    }

    file.addEventListener('change', async () => {
      const selected = file.files?.[0];
      if (!selected) return;
      file.value = '';
      if (!selected.size || selected.size > journalLimit) {
        notice.textContent = 'Choose a replay JSON file up to 2 MiB.';
        return;
      }
      stop();
      if (!controls.hidden) draw();
      file.disabled = true;
      notice.textContent = 'Loading replay…';
      try {
        if (!replay.prepare(await selected.arrayBuffer())) {
          notice.textContent = 'This replay file is invalid or incomplete.';
          return;
        }
        await loadArchive();
        if (replay.finish() !== 1 || replay.restart() !== 1) {
          notice.textContent = 'This replay could not be played.';
          return;
        }
        for (const item of [controls, position, canvas]) item.hidden = false;
        notice.textContent = 'Replay ready.';
        draw();
      } catch (_) {
        replay.cancel();
        notice.textContent = 'The replay’s recorded levels are unavailable or could not be verified.';
      } finally {
        file.disabled = false;
      }
    });
    play.addEventListener('click', guard(() => {
      if (running) stop();
      else {
        running = true;
        scheduled = requestAnimationFrame(frame);
      }
      draw();
    }));
    step.addEventListener('click', guard(() => {
      replay.step(1);
      if (replay.done() === 1) notice.textContent = 'Replay complete.';
      draw();
    }));
    reset.addEventListener('click', guard(() => {
      stop();
      replay.restart();
      notice.textContent = 'Replay ready.';
      draw();
    }));
    panel.closest('details')?.addEventListener('toggle', guard(event => {
      if (!event.currentTarget.open && running) {
        stop();
        draw();
      }
    }));
    file.disabled = false;
    panel.hidden = false;
  }
"#;
