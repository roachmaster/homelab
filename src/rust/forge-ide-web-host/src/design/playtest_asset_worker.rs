//! Build-time emitter for the playtest's bounded browser I/O transport.

#[derive(serde::Serialize)]
struct Endpoint {
    path: &'static str,
    limit: usize,
}

const ENDPOINTS: &[(&str, Endpoint)] = &[
    (
        "wasm",
        Endpoint {
            path: "/app.wasm",
            limit: 16 * 1024 * 1024,
        },
    ),
    (
        "manifest",
        Endpoint {
            path: "/playtest/assets/manifest.json",
            limit: 16 * 1024,
        },
    ),
    (
        "room-one",
        Endpoint {
            path: "/playtest/assets/levels/room-one.json",
            limit: 16 * 1024,
        },
    ),
    (
        "traversal-yard",
        Endpoint {
            path: "/playtest/assets/levels/traversal-yard.json",
            limit: 16 * 1024,
        },
    ),
    (
        "scout-atlas",
        Endpoint {
            path: "/playtest/assets/atlas/manifest.json",
            limit: 256 * 1024,
        },
    ),
    (
        "scout-atlas-image",
        Endpoint {
            path: "/playtest/assets/atlas/sheet.png",
            limit: 8 * 1024 * 1024,
        },
    ),
];

pub fn render() -> String {
    let config = serde_json::to_string(ENDPOINTS).expect("serialize playtest I/O endpoints");
    format!("'use strict';\nconst endpoints = new Map({config});\n{TRANSPORT}")
}

const TRANSPORT: &str = r#"
const pending = new Map();

async function fetchBytes({ path, limit }, signal) {
  const response = await fetch(path, {
    mode: 'same-origin', credentials: 'same-origin', redirect: 'error', signal,
  });
  if (!response.ok || !response.body) {
    if (response.body) await response.body.cancel().catch(() => {});
    throw new Error('unavailable');
  }
  if (Number(response.headers.get('content-length')) > limit) {
    await response.body.cancel().catch(() => {});
    throw new Error('too-large');
  }
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) throw new Error('too-large');
      chunks.push(value);
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  } finally {
    reader.releaseLock();
  }
  if (!length) throw new Error('unavailable');
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes.buffer;
}

function reject(request, asset, error) {
  postMessage({ kind: 'asset-error', request, asset, error });
}

self.onmessage = async ({ data: message }) => {
  const request = message?.request;
  if (!Number.isInteger(request) || request < 1 || request > 0xffffffff) return;
  if (message.kind === 'abort') {
    const entry = pending.get(request);
    pending.delete(request);
    entry?.controller.abort();
    return;
  }
  if (message.kind !== 'read' || pending.has(request)) return;
  const asset = message.asset;
  let endpoint = endpoints.get(asset);
  if (message.digest !== undefined) {
    if (typeof message.digest !== 'string' || !/^[a-f0-9]{64}$/.test(message.digest) ||
        !['manifest', 'room-one', 'traversal-yard'].includes(asset)) {
      reject(request, asset, 'unavailable');
      return;
    }
    const prefix = '/playtest/assets/';
    endpoint = { path: `${prefix}archive/${message.digest}/${endpoint.path.slice(prefix.length)}`, limit: endpoint.limit };
  }
  if (!endpoint || Array.from(pending.values()).some(entry => entry.path === endpoint.path)) {
    reject(request, asset, 'unavailable');
    return;
  }
  const controller = new AbortController();
  const entry = { path: endpoint.path, controller };
  pending.set(request, entry);
  try {
    const data = await fetchBytes(endpoint, controller.signal);
    if (controller.signal.aborted) reject(request, asset, 'aborted');
    else postMessage({ kind: 'asset-result', request, asset, data }, [data]);
  } catch (error) {
    reject(request, asset, controller.signal.aborted ? 'aborted' :
      error.message === 'too-large' ? 'too-large' : 'unavailable');
  } finally {
    if (pending.get(request) === entry) pending.delete(request);
  }
};
"#;
