//! Generated DOM, timing, and export primitives for the local playtest inspector.

pub(super) const DEBUG_ADAPTER: &str = r#"
  function attachDebugger(spec, player) {
    const drawer = document.querySelector(spec.drawer);
    const pause = document.querySelector(spec.pause);
    const step = document.querySelector(spec.step);
    const download = document.querySelector(spec.download);
    const overlay = document.querySelector(spec.overlay);
    const inspector = document.querySelector(spec.inspector);
    const tracePanel = document.querySelector(spec.trace);
    const timing = document.querySelector(spec.timing);
    const graph = document.querySelector(spec.graph);
    const feedback = document.querySelector(spec.feedback);
    const command = document.querySelector(spec.command);
    const commandInput = document.querySelector(spec.command_input);
    const commandRun = document.querySelector(spec.command_run);
    const channels = [...document.querySelectorAll(spec.channels)].map(control => [
      control, Number(control.getAttribute(spec.channel_attribute)),
    ]).filter(([, channel]) => Number.isInteger(channel) && channel >= 0 && channel < spec.channel_names.length);
    const buttons = [pause, step, download, overlay, commandInput, commandRun, ...channels.map(([control]) => control)];
    const samples = [];
    let paused = false;
    let lastRefresh = -Infinity;

    function show(message) {
      if (feedback) feedback.textContent = message;
    }

    function disable() {
      for (const control of buttons) if (control) control.disabled = true;
    }

    function syncControls(mask) {
      if (pause) {
        pause.setAttribute('aria-pressed', String(paused));
        pause.textContent = paused ? spec.resume_label : spec.pause_label;
      }
      if (step) step.disabled = !paused;
      for (const [control, channel] of channels) control.checked = (mask & (1 << channel)) !== 0;
    }

    function timingSummary() {
      const percentile = (ordered, p) => ordered[Math.max(0, Math.ceil(ordered.length * p) - 1)];
      return Object.keys(spec.scopes).map(scope => {
        const ordered = samples.map(sample => sample[scope]).sort((a, b) => a - b);
        return { scope, p50: percentile(ordered, 0.5), p95: percentile(ordered, 0.95), worst: ordered.at(-1) };
      });
    }

    function drawTiming() {
      const ctx = graph?.getContext('2d');
      if (!ctx) return;
      const { width, height } = graph;
      const max = Math.max(spec.budget_ms * 1.2, ...samples.map(sample => sample.frame));
      const yAt = value => height - Math.min(1, value / max) * height;
      ctx.clearRect(0, 0, width, height);
      ctx.strokeStyle = spec.budget_color;
      ctx.beginPath();
      ctx.moveTo(0, yAt(spec.budget_ms));
      ctx.lineTo(width, yAt(spec.budget_ms));
      ctx.stroke();
      ctx.strokeStyle = spec.sample_color;
      ctx.beginPath();
      samples.forEach((sample, i) => {
        const x = (spec.sample_capacity - samples.length + i) * width / spec.sample_capacity;
        if (i === 0) ctx.moveTo(x, yAt(sample.frame));
        else ctx.lineTo(x, yAt(sample.frame));
      });
      ctx.stroke();
    }

    function record(sample) {
      if (samples.length === spec.sample_capacity) samples.shift();
      samples.push(sample);
    }

    function present(snapshot, force) {
      const now = performance.now();
      if (!drawer?.open || (!force && now - lastRefresh < 100)) return;
      const trace = player.trace(snapshot.epoch);
      const entityViews = snapshotEntities(snapshot);
      const mainEntity = entityViews.find(entity => entity.kind === spec.player_kind);
      const eventViews = snapshotEvents(snapshot);
      const lines = [
        `Tick: ${snapshot.tick}`, `Run: ${snapshot.epoch}`,
        `Status: ${presentation.status_messages[snapshot.status]}`,
        `Entities: ${snapshot.entities}`, `Input queue: ${snapshot.inputDepth}`,
        `Ticks run: ${snapshot.ticksRun}`, `Ticks dropped: ${snapshot.ticksDropped}`,
        `Input edges dropped: ${snapshot.inputDropped}`, `Events: ${snapshot.events}`,
        `Events dropped: ${snapshot.eventsDropped}`, `WASM memory: ${player.memoryBytes()} bytes`,
        `Snapshot: ${snapshotBytes} bytes · Trace: ${traceBytes} bytes`,
      ];
      if (mainEntity) lines.push(`Player: (${mainEntity.x}, ${mainEntity.y}) · ID ${mainEntity.id}`);
      const pose = player.pose?.(snapshot);
      if (pose) {
        lines.push(`Pose: ${pose.clip} · frame ${pose.frame} · grounded: ${pose.grounded ? 'yes' : 'no'}`);
        if (pose.contact) {
          const { x, y, normal, speedQ16 } = pose.contact;
          const side = ['', 'left', 'right', 'up', 'down'][normal];
          lines.push(`Contact tile: (${x}, ${y}) · normal: ${side} · impact: ${speedQ16} Q16/tick`);
        }
      }
      for (const event of eventViews.slice(-4)) {
        lines.push(`${spec.event_names[event.kind] ?? event.kind} · target ${event.target} · (${event.x}, ${event.y})`);
      }
      if (inspector) inspector.textContent = lines.join('\n');
      const entries = traceRecords(trace, Math.max(0, trace.count - spec.visible_trace_records));
      const maskHex = trace.mask.toString(16).padStart(2, '0');
      const coverage = trace.replayComplete ? 'complete' : 'incomplete';
      const summary = `Trace records: ${trace.count} · Dropped: ${trace.dropped} · Mask: 0x${maskHex} · Replay: ${coverage}`;
      if (tracePanel) tracePanel.textContent = [summary, ...entries.map(record => {
        const phase = spec.phase_names[record.phase] ?? record.phase;
        const channel = spec.channel_names[record.channel] ?? record.channel;
        return `#${record.sequence} · tick ${record.tick} · ${phase}/${channel} · target ${record.target} · data ${record.data}`;
      })].join('\n');
      if (timing) {
        const showMs = ms => ms === undefined ? '—' : `${ms.toFixed(2)} ms`;
        timing.textContent = [`${samples.length} frames · ${spec.budget_ms.toFixed(2)} ms budget`, ...timingSummary().map(value =>
          `${spec.scopes[value.scope]} · p50 ${showMs(value.p50)} · p95 ${showMs(value.p95)} · worst ${showMs(value.worst)}`,
        )].join('\n');
      }
      drawTiming();
      lastRefresh = now;
    }

    function save(kind) {
      const snapshot = player.snapshot();
      const { view, ...trace } = player.trace(snapshot.epoch);
      const exportData = {
        version: 1, epoch: snapshot.epoch, tick: snapshot.tick,
        timing: { budgetMs: spec.budget_ms, samples, summary: timingSummary() },
      };
      if (kind === 'trace') {
        const activation = player.activation?.(snapshot);
        Object.assign(exportData, {
          replayComplete: trace.replayComplete,
          trace: { ...trace, records: traceRecords({ ...trace, view }) },
          events: snapshotEvents(snapshot),
          inputEdgesDropped: snapshot.inputDropped,
          eventsDropped: snapshot.eventsDropped,
        });
        if (activation) exportData.activation = activation;
      }
      const url = URL.createObjectURL(new Blob([JSON.stringify(exportData, null, 2)], { type: 'application/json' }));
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = kind === 'trace' ? spec.trace_filename : spec.timing_filename;
      anchor.hidden = true;
      document.body.append(anchor);
      try {
        anchor.click();
      } finally {
        anchor.remove();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
      show(kind === 'trace' ? spec.trace_saved : spec.timing_saved);
    }

    function setPaused(next) {
      if (next === paused) return;
      paused = next;
      player.clearInput();
      const snapshot = player.snapshot();
      syncControls(player.trace(snapshot.epoch).mask);
      player.refresh();
    }

    function stepOnce() {
      if (!paused) {
        show(spec.pause_first);
        return;
      }
      player.step();
      player.refresh();
    }

    function changeMask(mask) {
      player.traceChannels(mask);
      syncControls(mask);
      player.refresh();
    }

    function runCommand(value) {
      const [name, ...args] = value.trim().split(/\s+/);
      if (args.length === 0) {
        switch (name) {
          case 'pause': setPaused(true); return;
          case 'resume': setPaused(false); return;
          case 'step': stepOnce(); return;
          case 'restart': player.restart(); return;
          case 'trace': save('trace'); return;
          case 'timing': save('timing'); return;
        }
      }
      if (name === 'channels' && args.length === 1 && /^(?:\d{1,2}|0x[0-9a-f]{1,2})$/i.test(args[0])) {
        const mask = Number(args[0]);
        if (mask < (1 << spec.channel_names.length)) {
          changeMask(mask);
          return;
        }
      }
      show(spec.command_help);
    }

    for (const control of buttons) if (control) control.disabled = false;
    syncControls(player.trace(player.snapshot().epoch).mask);
    drawer?.addEventListener('toggle', player.refresh);
    pause?.addEventListener('click', player.guard(() => setPaused(!paused)));
    step?.addEventListener('click', player.guard(stepOnce));
    download?.addEventListener('click', player.guard(() => save('trace')));
    overlay?.addEventListener('change', player.refresh);
    for (const [control] of channels) control.addEventListener('change', player.guard(() => {
      changeMask(channels.reduce((mask, [box, channel]) => mask | (box.checked ? 1 << channel : 0), 0));
    }));
    command?.addEventListener('submit', player.guard(event => {
      event.preventDefault();
      if (commandInput) runCommand(commandInput.value);
    }));
    return {
      disable, record, present,
      paused: () => paused,
      overlay: () => Boolean(drawer?.open && overlay?.checked),
      shortcut(event, key) {
        if (!drawer?.open || event.repeat) return false;
        if (key === spec.pause_key) setPaused(!paused);
        else if (key === spec.step_key) stepOnce();
        else return false;
        return true;
      },
    };
  }
"#;
