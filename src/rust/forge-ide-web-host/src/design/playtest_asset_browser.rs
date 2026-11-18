//! Generated playtest I/O bridge and presentation-only atlas decoding.

pub(super) const ASSET_ADAPTER: &str = r#"
  function workerAssetIo() {
    const worker = new Worker('/playtest/assets-worker.js');
    const pending = new Map();
    let latest = 0;
    let closed = false;
    function take(request) {
      const entry = pending.get(request);
      if (!entry) return;
      pending.delete(request);
      entry.signal?.removeEventListener('abort', entry.abort);
      return entry;
    }
    function close() {
      if (closed) return;
      closed = true;
      worker.terminate();
      for (const request of pending.keys()) take(request).reject(new Error('Playtest I/O worker closed'));
    }
    worker.onerror = close;
    worker.onmessageerror = close;
    worker.onmessage = ({ data: message }) => {
      const entry = pending.get(message?.request);
      if (!entry) return;
      take(message.request);
      if (message.asset === entry.asset && message.kind === 'asset-result' &&
          message.data instanceof ArrayBuffer && message.data.byteLength) {
        entry.resolve(message.data);
      } else {
        entry.reject(new Error('Playtest asset unavailable'));
      }
    };

    function read(asset, signal, digest) {
      return new Promise((resolve, reject) => {
        if (closed || latest === 0xffffffff) throw new Error('Playtest I/O worker unavailable');
        signal?.throwIfAborted();
        const request = ++latest;
        const abort = () => {
          const entry = take(request);
          if (!entry) return;
          worker.postMessage({ kind: 'abort', request });
          entry.reject(signal.reason);
        };
        pending.set(request, { asset, signal, abort, resolve, reject });
        signal?.addEventListener('abort', abort, { once: true });
        try {
          worker.postMessage({ kind: 'read', request, asset, digest });
        } catch (error) {
          take(request);
          reject(error);
        }
      });
    }

    return { read, close };
  }

  function assetAdmission(get, memory) {
    const operations = [
      'level_request', 'asset_reserve', 'asset_accept', 'atlas_view', 'atlas_view_len',
      'level_collider', 'level_fail',
    ].map(get);
    if (operations.some(operation => typeof operation !== 'function')) return null;
    const [request, reserve, accept, atlasPtr, atlasLen, collider, fail] = operations;
    const decoder = new TextDecoder('utf-8', { fatal: true });
    return {
      request,
      fail,
      accept(asset, generation, bytes) {
        const length = bytes.byteLength;
        const offset = reserve(asset, length) >>> 0;
        if (!offset || offset + length > memory.buffer.byteLength) {
          throw new Error('Playtest asset upload rejected');
        }
        new Uint8Array(memory.buffer, offset, length).set(new Uint8Array(bytes));
        if (accept(asset, generation) !== 1) throw new Error('Playtest asset rejected');
      },
      collider: () => [0, 1, 2, 3].map(edge => collider(edge)),
      atlas() {
        const offset = atlasPtr() >>> 0;
        const length = atlasLen() >>> 0;
        if (!offset || !length || offset + length > memory.buffer.byteLength) {
          throw new Error('Playtest atlas view unavailable');
        }
        return JSON.parse(decoder.decode(new Uint8Array(memory.buffer, offset, length)));
      },
    };
  }

  function cookedAssets(admission, spec, io) {
    let manifestReady = false;
    async function ensureManifest(signal) {
      if (manifestReady) return;
      const bytes = await io.read('manifest', signal);
      signal.throwIfAborted();
      admission.accept(0, 0, bytes);
      manifestReady = true;
    }

    async function loadLevel(choice, signal) {
      const request = admission.request(choice.level) >>> 0;
      if (!request) throw new Error('Playtest level request rejected');
      try {
        await ensureManifest(signal);
        const bytes = await io.read(choice.asset, signal);
        signal.throwIfAborted();
        admission.accept(choice.level, request, bytes);
      } catch (error) {
        admission.fail(request);
        throw error;
      }
    }

    async function loadAtlas(signal) {
      await ensureManifest(signal);
      const [atlasBytes, imageBytes] = await Promise.all([
        io.read(spec.atlas_id, signal), io.read(spec.image_id, signal),
      ]);
      signal.throwIfAborted();
      admission.accept(3, 0, atlasBytes);
      admission.accept(4, 0, imageBytes);
      const view = admission.atlas();
      const image = await createImageBitmap(new Blob([imageBytes], { type: 'image/png' }));
      if (signal.aborted || image.width !== view.dimensions[0] || image.height !== view.dimensions[1]) {
        image.close();
        throw new Error('Playtest atlas decode did not match the verified dimensions');
      }
      return { image, clips: new Map(Object.entries(view.clips)), pivot: view.pivot, size: view.size };
    }

    return { loadLevel, loadAtlas };
  }
"#;
