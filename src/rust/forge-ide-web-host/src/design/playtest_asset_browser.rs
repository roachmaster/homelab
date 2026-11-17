//! Generated browser adapter for bounded, same-origin playtest asset reads.

pub(super) const ASSET_ADAPTER: &str = r#"
  function cookedAssets(spec) {
    const decoder = new TextDecoder('utf-8', { fatal: true });
    const paths = new Map(spec.entries);
    let manifest;

    async function bytes(path, limit, signal) {
      const response = await fetch(path, {
        mode: 'same-origin', credentials: 'same-origin', redirect: 'error', signal,
      });
      const length = response.headers.get('content-length');
      if (!response.ok || !response.body || (length !== null && Number(length) > limit)) {
        throw new Error('Cooked playtest asset unavailable');
      }
      const reader = response.body.getReader();
      const chunks = [];
      let total = 0;
      try {
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          total += value.byteLength;
          if (total > limit) throw new Error('Cooked playtest asset too large');
          chunks.push(value);
        }
      } catch (error) {
        await reader.cancel().catch(() => {});
        throw error;
      } finally {
        reader.releaseLock();
      }
      if (total === 0) throw new Error('Empty cooked playtest asset');
      const output = new Uint8Array(total);
      let offset = 0;
      for (const chunk of chunks) {
        output.set(chunk, offset);
        offset += chunk.byteLength;
      }
      return output;
    }

    async function digest(data) {
      return new Uint8Array(await crypto.subtle.digest('SHA-256', data));
    }

    async function readManifest(signal) {
      if (manifest) return manifest;
      const raw = await bytes(spec.base + 'manifest.json', spec.metadata_limit, signal);
      const { version, entries } = JSON.parse(decoder.decode(raw));
      if (version !== 1 || !entries || Array.isArray(entries) || typeof entries !== 'object') {
        throw new Error('Invalid cooked playtest manifest');
      }
      const checked = new Map();
      for (const [id, entry] of Object.entries(entries)) {
        const expected = paths.get(id);
        if (!expected || !entry || entry.kind !== expected.kind || entry.path !== expected.path ||
            typeof entry.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(entry.sha256)) {
          throw new Error('Invalid cooked playtest asset entry');
        }
        for (const list of [entry.dependencies, entry.optional_dependencies]) {
          if (!Array.isArray(list) || list.length > paths.size ||
              list.some(dependency => typeof dependency !== 'string' ||
                !paths.has(dependency)) || new Set(list).size !== list.length) {
            throw new Error('Invalid cooked playtest dependencies');
          }
        }
        checked.set(id, entry);
      }
      const hash = await digest(raw);
      signal.throwIfAborted();
      manifest = { entries: checked, hash };
      return manifest;
    }

    async function readAsset(id, signal) {
      const { entries, hash } = await readManifest(signal);
      const fetched = new Map();
      const visiting = new Set();
      async function read(assetId) {
        if (fetched.has(assetId)) return fetched.get(assetId);
        const entry = entries.get(assetId);
        if (!entry || visiting.has(assetId)) throw new Error('Unresolved cooked playtest dependency');
        visiting.add(assetId);
        for (const dependency of entry.dependencies) await read(dependency);
        const limit = entry.kind === 'image' ? spec.image_limit :
          entry.kind === 'level' ? spec.level_limit : spec.metadata_limit;
        const data = await bytes(spec.base + entry.path, limit, signal);
        const actual = Array.from(await digest(data), value => value.toString(16).padStart(2, '0')).join('');
        if (actual !== entry.sha256) throw new Error('Cooked playtest digest mismatch');
        fetched.set(assetId, data);
        visiting.delete(assetId);
        return data;
      }
      return { data: await read(id), hash, dependencies: fetched };
    }

    async function loadLevel(id, signal) {
      const cooked = await readAsset(id, signal);
      const chunk = JSON.parse(decoder.decode(cooked.data));
      if (!chunk || chunk.id !== id) throw new Error('Cooked playtest level ID mismatch');
      const collider = ['left', 'top', 'right', 'bottom'].map(edge => chunk.collider?.[edge]);
      if (!collider.every(value => Number.isInteger(value) && Math.abs(value) <= 65536)) {
        throw new Error('Cooked playtest collider out of range');
      }
      return { ...cooked, collider };
    }

    async function loadAtlas(signal) {
      const { data, dependencies } = await readAsset(spec.atlas_id, signal);
      const imageBytes = dependencies.get(spec.image_id);
      const atlas = JSON.parse(decoder.decode(data));
      const pair = (value, minimum, maximum) => Array.isArray(value) && value.length === 2 &&
        value.every(number => Number.isInteger(number) && number >= minimum && number <= maximum);
      if (!imageBytes || atlas.version !== 2 || atlas.image !== 'sheet.png' ||
          !Number.isInteger(atlas.atlas_scale) || atlas.atlas_scale < 1 || atlas.atlas_scale > 8 ||
          !pair(atlas.logical_frame_size, 1, 128) || !pair(atlas.frame_size, 1, 1024) ||
          !pair(atlas.pivot, 0, 127) || !atlas.clips || typeof atlas.clips !== 'object' ||
          atlas.frame_size.some((value, axis) => value !== atlas.logical_frame_size[axis] * atlas.atlas_scale) ||
          atlas.pivot.some((value, axis) => value >= atlas.logical_frame_size[axis])) {
        throw new Error('Invalid cooked playtest atlas');
      }
      const header = new DataView(imageBytes.buffer, imageBytes.byteOffset, imageBytes.byteLength);
      if (header.byteLength < 24 || header.getUint32(0) !== 0x89504e47 || header.getUint32(4) !== 0x0d0a1a0a ||
          header.getUint32(12) !== 0x49484452) throw new Error('Invalid playtest atlas image');
      const width = header.getUint32(16), height = header.getUint32(20);
      const images = [width, height];
      if (!pair(images, 1, 4096)) throw new Error('Playtest atlas dimensions out of range');
      const clips = new Map();
      const [frameWidth, frameHeight] = atlas.frame_size;
      for (const key of new Set(spec.pose_clips)) {
        const frames = atlas.clips[key];
        if (!Array.isArray(frames) || frames.length < 1 || frames.length > 512) {
          throw new Error('Invalid cooked playtest clip');
        }
        clips.set(key, frames.map(({ rect }) => {
          if (!Array.isArray(rect) || rect.length !== 4 ||
              rect.some(number => !Number.isInteger(number) || number < 0) ||
              rect[2] !== frameWidth || rect[3] !== frameHeight ||
              rect[0] + frameWidth > width || rect[1] + frameHeight > height) {
            throw new Error('Invalid cooked playtest atlas rectangle');
          }
          return rect;
        }));
      }
      const image = await createImageBitmap(new Blob([imageBytes], { type: 'image/png' }));
      if (signal.aborted || image.width !== width || image.height !== height) {
        image.close();
        throw new Error('Playtest atlas decode did not match its header');
      }
      return { image, clips, pivot: atlas.pivot, size: atlas.logical_frame_size };
    }

    return { loadLevel, loadAtlas };
  }

  function levelAdmission(get) {
    const operations = ['level_begin', 'level_manifest_word', 'level_byte', 'level_prepare', 'level'].map(get);
    if (operations.some(operation => typeof operation !== 'function')) return null;
    const [begin, word, byte, prepare, active] = operations;
    let generation = 0;
    return {
      active,
      stage({ data, hash }) {
        if (generation === 0xffffffff) throw new Error('Playtest level requests exhausted');
        const request = ++generation;
        const view = new DataView(hash.buffer, hash.byteOffset, hash.byteLength);
        if (begin(request, view.getUint32(0, true)) !== 1) throw new Error('Playtest level request rejected');
        for (let index = 0; index < 8; index++) {
          if (word(request, index, view.getUint32(index * 4, true)) !== 1) {
            throw new Error('Playtest manifest identity rejected');
          }
        }
        for (const value of data) {
          if (byte(request, value) !== 1) throw new Error('Playtest level bytes rejected');
        }
        if (prepare(request) !== 1) throw new Error('Playtest level validation failed');
      },
    };
  }
"#;
