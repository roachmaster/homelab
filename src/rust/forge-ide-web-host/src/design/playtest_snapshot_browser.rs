//! Generated readers for the playtest's versioned, little-endian WASM views.

pub(super) const SNAPSHOT_READERS: &str = r#"
  const snapshotBytes = 10284;
  const traceBytes = 8216;

  function memoryView(memory, getOffset, getLength, length, label) {
    let view;
    return () => {
      const offset = getOffset() >>> 0;
      const actualLength = getLength() >>> 0;
      const bytes = memory.buffer;
      if (offset === 0 || actualLength !== length || offset > bytes.byteLength - length) {
        throw new Error(`${label} buffer is out of range`);
      }
      if (!view || view.buffer !== bytes || view.byteOffset !== offset) {
        view = new DataView(bytes, offset, length);
      }
      return view;
    };
  }

  let boardView;
  let boardCells;

  function readSnapshot(view) {
    const width = view.getUint16(6, true);
    const height = view.getUint16(8, true);
    const entities = view.getUint16(10, true);
    const events = view.getUint16(12, true);
    const fraction = view.getUint32(24, true);
    const epoch = view.getUint32(16, true);
    const status = view.getUint8(28);
    const ticksRun = view.getUint8(29);
    if (view.getUint32(0, true) !== 0x31534746 || view.getUint16(4, true) !== 1 ||
        width < 1 || height < 1 || width > 64 || height > 64 || entities > 256 || events > 64 ||
        fraction >= 1000000 || epoch === 0 || status >= presentation.status_messages.length ||
        ticksRun > 4 || view.getUint16(30, true) !== 0) {
      throw new Error('Invalid playtest snapshot');
    }
    if (boardView !== view || boardCells.length !== width * height) {
      boardView = view;
      boardCells = new Uint8Array(view.buffer, view.byteOffset + 32, width * height);
    }
    return {
      view, width, height, entities, events, fraction, epoch, status, ticksRun,
      tick: view.getUint32(20, true),
      inputDepth: view.getUint16(14, true),
      ticksDropped: view.getUint32(10272, true),
      inputDropped: view.getUint32(10276, true),
      eventsDropped: view.getUint32(10280, true),
      cells: boardCells,
    };
  }

  function snapshotEntities(snapshot) {
    const { view, entities } = snapshot;
    return Array.from({ length: entities }, (_, index) => {
      const offset = 4128 + index * 20;
      return {
        id: view.getUint16(offset, true),
        kind: view.getUint8(offset + 2),
        state: view.getUint8(offset + 3),
        x: view.getInt32(offset + 4, true) / 256,
        y: view.getInt32(offset + 8, true) / 256,
        previousX: view.getInt32(offset + 12, true) / 256,
        previousY: view.getInt32(offset + 16, true) / 256,
      };
    });
  }

  function snapshotEvents(snapshot) {
    const { view, events, width, height } = snapshot;
    let previous = 0;
    return Array.from({ length: events }, (_, index) => {
      const offset = 9248 + index * 16;
      const sequence = view.getUint32(offset, true);
      const tick = view.getUint32(offset + 4, true);
      const x = view.getUint16(offset + 12, true);
      const y = view.getUint16(offset + 14, true);
      if (sequence <= previous || tick > snapshot.tick || x >= width || y >= height ||
          view.getUint8(offset + 11) !== 0) {
        throw new Error('Invalid playtest event');
      }
      previous = sequence;
      return { sequence, tick, target: view.getUint16(offset + 8, true), kind: view.getUint8(offset + 10), x, y };
    });
  }

  function readTrace(view, epoch) {
    const count = view.getUint16(6, true);
    const latest = view.getUint32(12, true);
    const flags = view.getUint8(23);
    const mask = view.getUint32(20, true) & 0xffffff;
    if (view.getUint32(0, true) !== 0x31544746 || view.getUint16(4, true) !== 1 ||
        count > 512 || count > latest || view.getUint32(8, true) !== epoch ||
        (flags & ~1) !== 0 || (mask & ~63) !== 0) {
      throw new Error('Invalid playtest trace');
    }
    return { view, count, epoch, latest, mask, dropped: view.getUint32(16, true), replayComplete: (flags & 1) !== 0 };
  }

  function traceRecords(trace, first = 0) {
    const { view, count, latest } = trace;
    return Array.from({ length: count - first }, (_, index) => {
      const entry = first + index;
      const offset = 24 + entry * 16;
      const sequence = view.getUint32(offset, true);
      if (sequence !== latest - count + entry + 1) throw new Error('Invalid playtest trace sequence');
      return {
        sequence,
        tick: view.getUint32(offset + 4, true),
        phase: view.getUint8(offset + 8),
        channel: view.getUint8(offset + 9),
        target: view.getUint16(offset + 10, true),
        data: view.getUint32(offset + 12, true),
      };
    });
  }
"#;
