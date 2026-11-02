# Forge IDE UI model

Portable Rust policy for the read-only Forge IDE inspection UI. This package has
only `serde` as a normal dependency: no native inspection, filesystem, DOM,
network, TLS or asynchronous runtime. It is intentionally a pure model, not an
empty DI container or an additional implementation of `Task`.

`UiModel::dispatch(Input)` accepts typed application events and returns typed
`Output { view, effects, error }`. The actual worker-only ABI lives in
[`forge-ide-web/src/wasm.rs`](../forge-ide-web/src/wasm.rs). JSON is that boundary's
codec, not internal state. The same model runs in native unit tests and real
`wasm32-unknown-unknown` code.

## Ownership

- Rust owns node/project selections, request epochs/revisions/IDs, active request,
  deadline acceptance, report validation, tree expansion, outline/details and
  summary counts. One active inspection; explicit retry creates a new request.
- Generated worker glue performs prepared `Resume`, `Pair`, `Inspect` and `Abort`
  effects. It retains CSRF only as transport metadata. A pairing code is emitted
  once as an effect and is not stored in the model or view; this is not a claim
  of cryptographic memory erasure in Rust or JavaScript.
- Generated main-thread primitives apply the generic `ViewModel` bindings as text
  and semantic controls, preserving focus/browser lifecycle. No domain reducer,
  source-derived HTML, backend fetch or application-specific JavaScript is needed.
- Host and agent independently authorize every request. Catalog choices, view IDs,
  browser request IDs and this model are **not authentication or authority**.

`Resume` or `Pair` starts a matching request. Only matching `Paired` initializes
an authorized catalog. Inspection effects carry configured node identity, alias,
version, expiry and a `{session_epoch, revision, request_id}` token. Selection
changes or Cancel invalidate ownership before emitting local abort; delayed
results are ignored. Cancellation/expiry means local waiting stopped, not that
native parsing or remote I/O was preempted. Failed effects never auto-retry.

## Display contract and limits

The host projects the existing inspection report into `ReportView`: flat preorder
entries and per-file declaration forests, numeric IDs, tagged native names,
line counts, diagnostics, spans and implementation labels. No file body or full
expression AST is present. Labels/type spellings/diagnostics can expose project
metadata; this is not secret redaction. Non-UTF-8 names are explicit hexadecimal
labels, not lossy replacement strings; IDs, not labels, address view actions.

The validator requires versions/IDs/parents/preorder/counts/spans to agree, rejects
unknown or inconsistent file-status fields, and forbids `complete: true` when
unreadable/too-large/unparsed-Rust entries prove incompleteness. It accepts at
most 4,096 entries, 16,384 declarations, hierarchy depth 64, 4 KiB per text/name,
2 MiB aggregate report text and 16 diagnostics per entry. An unreadable root is
represented honestly as a one-entry incomplete report. Catalogs have at most
32 nodes and 128 aliases per node. Tree and outline render in 256-row increments;
the bounded report remains intact. These are deliberate UI limits, not silent
truncation of an otherwise claimed complete report.

The WASM boundary caps input and serialized output at 8 MiB. Domain checks happen
after bounded byte decoding, not in a streaming allocation-proof decoder. The
allocator, browser and WASM engine still need resources; trusted finite bounds do
not create a security sandbox. Clock values come from the worker and must fit an
exact JavaScript integer; the host independently checks its own deadline. Local
request IDs are correlation tokens, not globally unique deduplication keys.

## ABI and verification

Worker order: `abi_version()` → `reserve_input(len)` → copy JSON bytes into the
exported memory → `dispatch_input()` → copy `output_ptr()/output_len()` bytes →
`release_output()`. Refresh memory views after calls/growth; never retain a view
across a call or await. ABI zero means stop that instance: no partial output or
effect is exposed, but the model may already have changed. The facade uses owned
buffers and never reconstructs a slice from a caller-supplied pointer. Exported
linker-name attributes are the only unsafe-code lint allowance; there are no
unsafe blocks. This remains a trusted generated worker ABI, not a malicious-host
memory isolation layer.

```sh
cargo test --manifest-path ../Cargo.toml -p forge-ide-ui
cargo build --manifest-path ../Cargo.toml -p forge-ide-web --lib \
  --target wasm32-unknown-unknown --release
node tests/wasm_abi.mjs ../target/wasm32-unknown-unknown/release/forge_ide_web.wasm
node tests/worker_glue.mjs /path/to/generated/assets
```

The first Node harness tests the **actual built WASM**, not browser DOM/network
behavior. The second loads generated `worker.js` with that WASM and simulated
fetch/ABI faults to verify HTTP error classification, local cancellation/stale
results, successful typed preparation and fail-closed instance lifecycle. Its
JavaScript is test instrumentation, never a shipped application asset.
Native tests cover state transitions, stale/cancel/error handling, malformed
wire input, projection bounds, native names and declaration hierarchy. Browser
journeys belong to the composition's separate integration checks.
