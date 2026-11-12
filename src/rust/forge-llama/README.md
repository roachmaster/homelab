# Forge llama.cpp adapter

`forge-llama` is a loopback REST service for one local llama.cpp server. The adapter starts with the GPU idle. Select an allowlisted model to start llama.cpp or replace the currently loaded model. Send chat messages to the selected model and receive the text, token usage and available timing fields. The Forge GPU configuration selects the Arc B50 through SYCL.

The [GPT-OSS 20B KV cache benchmark](KV-CACHE-BENCHMARK.md) records the memory, throughput, and retrieval results for FP16 and Q8_0 caches on Forge.

## REST contract

Every endpoint requires `Authorization: Bearer <token>`. JSON bodies require `Content-Type: application/json`.

| Request | Effect or result |
|---|---|
| `GET /v1/models` | List configured model IDs, including unloaded models |
| `GET /v1/runtime` | Report `stopped` or the live, previously ready model |
| `PUT /v1/runtime` with `{"model":"gpt-oss-20b"}` | Start or switch models and return after readiness. Repeating the active selection is idempotent. |
| `POST /v1/chat/completions` | Submit a stateless non-streaming chat. An optional `model` must match the active ID. |
| `DELETE /v1/runtime` | Stop and reap the owned llama.cpp process. Repeating the request succeeds. |

Chat supports text `system`, `user` and `assistant` messages. Supply previous messages for a conversation. The request accepts up to 64 messages, 64 KiB of text and 2,048 completion tokens. The adapter allows one command at a time. While a command is active, a second command returns HTTP 409. Invalid or unlisted models are rejected before changing the current model. If loading fails after the old process is stopped, the runtime is stopped. A transport failure or deadline can stop the model to release an unfinished upstream prompt. Clients explicitly restart it with PUT.

Access Forge remotely with an SSH tunnel. Read the token over an authorized SSH session or use it directly on Forge. Keep the token out of source control and use an environment variable in your shell.

```bash
ssh -N -L 18080:127.0.0.1:18080 forge-gpu
```

In a separate shell:

```bash
export FORGE_LLAMA_TOKEN="$(ssh forge-gpu 'cat ~/.config/forge-llama/token')"
curl -fsS -H "Authorization: Bearer $FORGE_LLAMA_TOKEN" http://127.0.0.1:18080/v1/models
curl -fsS -X PUT -H "Authorization: Bearer $FORGE_LLAMA_TOKEN" -H 'Content-Type: application/json' \
  -d '{"model":"gpt-oss-20b"}' http://127.0.0.1:18080/v1/runtime
curl -fsS -H "Authorization: Bearer $FORGE_LLAMA_TOKEN" -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"What is 6 times 7?"}],"max_tokens":256}' \
  http://127.0.0.1:18080/v1/chat/completions
curl -fsS -X DELETE -H "Authorization: Bearer $FORGE_LLAMA_TOKEN" http://127.0.0.1:18080/v1/runtime
unset FORGE_LLAMA_TOKEN
```

## Run on Forge

Build with current stable Rust and the workspace lockfile:

```bash
cargo +stable build --release --locked --manifest-path src/rust/Cargo.toml -p forge-llama
```

Use the [Forge configuration example](deploy/forge-gpu.example.json) with actual absolute paths. Place the tested llama.cpp executable and its shared libraries together at the configured server path. The SYCL runtime requires the Intel loader library directories shown in the example. Keep the GPU lock path shared with cooperating benchmark launchers. Generate a 32-byte random hex token in a mode-0600 file before starting. Install the built adapter at `~/.local/bin/forge-llama` and its configuration at `~/.config/forge-llama/config.json`.

### Start the API at boot

Copy [setup-boot.sh](deploy/setup-boot.sh) and [forge-llama.service](deploy/forge-llama.service)
together to the inference host. Run the script there as the account that owns
the adapter. The account needs sudo access to enable systemd user lingering.

```bash
./setup-boot.sh
./setup-boot.sh --check
```

The script enables and starts the API and checks the authenticated loopback
endpoint. Re-running it preserves an active adapter and its loaded model.
The user service starts at boot without an SSH login. A fresh adapter starts
with the GPU idle. Use `PUT /v1/runtime` to select a model, and
`journalctl --user -u forge-llama` to view service logs.

The adapter and backend bind IPv4 loopback. The backend uses the same local bearer token file, and the deployment assumes trusted access to the GPU host. The shared GPU lock coordinates cooperating launchers. Stop any unrelated workload that owns the GPU using its own controls before loading a model. The service is independent of the legacy system service.

## Ownership and verification

See the [C4 and UML architecture views](ARCHITECTURE.md) for the system boundary, worker and driver, model switching, and chat continuation.

The method and path choose the Forge route. JSON is decoded into a typed request at the REST boundary. The driver owns child process creation, model readiness and the GPU lock. The server owns tokenizer, prompt template, inference and decoding. The package exports `Config`, `register_llama`, `LlamaFactory`, `Request` and `register_routes`. The REST executable consumes those exports. Registration stages a configuration before the host freezes and constructs its singleton. The standalone [consumer fixture](tests/consumer/Cargo.toml) exercises those public imports, duplicate rejection, admission and payload mismatch without loading a model.

API references were checked through Context7, then against Forge's pinned llama.cpp build. [Upstream server documentation](https://github.com/ggml-org/llama.cpp/tree/master/tools/server)

The [child lifecycle check](tests/owned_child.py) exercises the running REST binary against a local test server. It verifies chat, SIGTERM on model replacement, failure of a replacement, and child shutdown when the adapter exits. The [Forge smoke test](tests/forge_smoke.py) loads both real models through the REST API and returns the runtime to stopped. Run that test only when the GPU is available and no model is loaded.

## Forge deployment and incident record

The user service was installed on Forge with the preserved SYCL b12110 build. The configured PCI device is `0000:03:00.0` and the REST port is `18080`. The boot setup script enables the user service, and clients select a model with PUT.

The initial hardware run loaded Qwen 2.5 Coder 7B, switched to GPT-OSS 20B and received `42` from both through REST. GPU engine resets appeared around the initial forced process exits. The adapter now sends SIGTERM and waits up to eight seconds before a forced cleanup. A subsequent SYCL load hit an `xe` wedge, and a device-only rebind blocked on a GPU fence. The adapter remains reachable and rejects model startup with `503 gpu_unavailable` while the PCI driver or render node is absent. The test child verified the revised lifecycle. A fresh hardware run is still needed after a Forge power cycle. [Linux Xe device wedging](https://docs.kernel.org/gpu/xe/xe_device.html)
