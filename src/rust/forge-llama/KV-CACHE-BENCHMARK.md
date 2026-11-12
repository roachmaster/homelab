# GPT-OSS 20B KV cache benchmark on Forge

On 2026-11-11, FP16 and Q8_0 both processed a **34,494-token input** and returned all three exact references in a **49,152-token context window**. Q8_0 saved **543 MiB of KV cache** and **453 MiB of sampled peak GPU memory** at that allocation. FP16 finished the request in **114.5 seconds**, compared with **124.9 seconds** for Q8_0. This run supports keeping FP16 as the IDE default and evaluating Q8_0 when GPU memory becomes the constraint.

## Test setup

- Forge used an Intel Arc Pro B50 with 16 GiB VRAM, PCI device `0000:03:00.0`, the Linux `xe` driver on kernel `7.3.0-10-generic`, and an AMD Ryzen 7 7700X.
- The installed llama.cpp SYCL build was **b12110** (`cd959bae9`), compiled with `GGML_SYCL_F16=ON`. All four configurations offloaded **25/25 layers** of the same GPT-OSS 20B MXFP4 weights to `SYCL0`.
- Both cache formats used Flash Attention, one sequence, eight CPU threads, logical and physical batch sizes of 128, low reasoning effort, greedy sampling, and a 1,024-token generation cap. The cache type applied to both K and V. Prompt reuse and context shifting were disabled.
- At 32K, each cache format ran the original 13,689-token inventory retrieval prompt twice in a fresh server. The second request reused the server process and recomputed the full prompt. At 48K, each cache format ran a fresh 34,494-token inventory retrieval prompt once. The rendered prompt and token IDs matched between formats at each context size. Run order was 32K FP16, 32K Q8_0, 48K Q8_0, then 48K FP16.

## Memory

| Context allocation | Cache format | KV buffers | Peak resident GPU memory |
|---|---|---:|---:|
| 32,768 | FP16 | 774.00 MiB | 11,996 MiB |
| 32,768 | Q8_0 | 411.19 MiB | 11,687 MiB |
| 49,152 | FP16 | 1,158.00 MiB | 12,380 MiB |
| 49,152 | Q8_0 | 615.19 MiB | 11,927 MiB |

KV buffer sizes are from the server log. Peak resident memory is sampled from the server's GPU allocation counters every 250 ms and includes the model and working buffers. Q8_0 reduced the cache buffers by about 47% at both sizes. The sampled process peak dropped by 309 MiB at 32K and 453 MiB at 48K.

## Throughput and retrieval

| Actual input tokens | Request | FP16 prompt / generation (tok/s) | Q8_0 prompt / generation (tok/s) | FP16 / Q8_0 response (s) |
|---:|---|---:|---:|---:|
| 13,689 | First | 291.2 / 55.8 | 255.0 / 53.8 | 48.9 / 55.6 |
| 13,689 | Second | 450.1 / 56.0 | 460.8 / 53.4 | 32.3 / 31.7 |
| 34,494 | First | 311.7 / 47.9 | 285.3 / 45.5 | 114.5 / 124.9 |

The rates come from llama.cpp's prompt and generation timings. Response time is the full loopback HTTP request and excludes server startup. Each 32K request generated 105 tokens and each 48K request generated 182. All six requests stopped normally, reported zero reused input tokens, and returned exactly the expected JSON object. The returned content and parsed reasoning text matched between cache formats for each paired request.

Prompt throughput varied between the first and second requests at 32K. The 48K result is a single observation per format. The test covers exact retrieval of three values from each synthetic inventory, and the longest input occupied 34,494 of the 49,152 allocated positions.

## Evidence

Mission Control holds the run summary, topology, per-condition settings, synthetic prompts and expected answers, raw responses, and server logs under `/mnt/data/forge-homelab/model-downloads/kv-ab-20261111T233305Z-x2ksszuv/`. The saved summary confirms the live adapter returned to `ready` with GPT-OSS 20B after the test.
