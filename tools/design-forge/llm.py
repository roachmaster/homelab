#!/usr/bin/env python3
"""Timed, policy-enforced calls to the local llama-server.

Standing rule (Leonardo, 2026-11-26): every request to the local model
is timed, and the harness - not hope - decides when the model's memory
gets cleared and a request retried.

Observed baseline: Qwen2.5-Coder-7B on forge-gpu generates at ~40 tok/s;
a design packet is 125-600 tokens and a file is capped at 1,400 output
tokens, so a legitimate request finishes in well under a minute. The
budgets below carry ~3x headroom over that worst case:

- WARN_S (60s): the response is logged as slow.
- ABORT_S (120s): the request is cut off. Past this point the model is
  looping, not working.

Hallucination tripwires (checked mechanically on every response):
- a line repeated 6+ times in a row (the loop signature);
- finish_reason == "length" (ran to the token cap mid-thought);
- `use crate::<module>` naming a module with no card in the design
  store (an invented collaborator - the kennel failure mode).

On a timeout or any tripwire: clear the model's memory (best-effort
llama-server slot erase; the harness side restarts from the bare packet
with no accumulated conversation), retry ONCE, and if it fails again the
target is blocked and surfaced to the operator. Never retry in a loop.

Every request appends one JSON line to out/llm-log.jsonl:
{ts, label, elapsed_s, prompt_tokens, resp_chars, finish_reason, outcome}
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
API = "http://127.0.0.1:8080/v1/chat/completions"
SLOTS_API = "http://127.0.0.1:8080/slots"
MODEL = "qwen2.5-coder-7b-instruct-q4_k_m.gguf"
MAX_TOKENS = int(os.environ.get("DF_MAX_TOKENS", "1400"))
WARN_S = 60.0
ABORT_S = 120.0
LOG = os.path.join(HERE, "out", "llm-log.jsonl")


def set_out_dir(path):
    """Point the request log at the active run's out dir."""
    global LOG
    LOG = os.path.join(path, "llm-log.jsonl")


class LlmTimeout(Exception):
    kind = "timeout"

    def __init__(self, elapsed):
        super().__init__(f"no response within {elapsed:.0f}s")
        self.elapsed = elapsed


class LlmHallucination(Exception):
    def __init__(self, kind, detail, elapsed):
        super().__init__(f"{kind}: {detail}")
        self.kind = kind
        self.elapsed = elapsed


def _log(record):
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    with open(LOG, "a") as f:
        f.write(json.dumps(record) + "\n")


def _loop_line(text):
    """The first line repeated 6+ times consecutively, or None."""
    prev, run = None, 0
    for line in text.splitlines():
        s = line.strip()
        if s and s == prev:
            run += 1
            if run >= 6:
                return s
        else:
            prev, run = s, 0
    return None


def _invented_modules(text, known_modules):
    if known_modules is None:
        return []
    used = set(re.findall(r"use\s+crate::([a-z_][a-z0-9_]*)", text))
    return sorted(used - set(known_modules))


def clear_memory(label):
    """Best-effort KV-cache clear on the server side (slot erase).

    The harness side of clearing is the caller's: restart the target
    from the bare packet, with none of the accumulated conversation.
    A failed erase is logged WITH its HTTP status and response body,
    never silently - a broken erase (e.g. the server started without
    --slots/--slot-save-path, which answers 501) must fail loudly,
    because the retry otherwise proceeds on a warm KV cache believing
    memory was cleared. Still never fatal: a fresh packet with no
    shared prefix already busts the prompt cache in practice.
    """
    status, body = None, ""
    try:
        req = urllib.request.Request(f"{SLOTS_API}/0?action=erase",
                                     data=b"", method="POST")
        with urllib.request.urlopen(req, timeout=5) as r:
            status = r.status
            body = r.read().decode("utf-8", "replace")[:200]
    except urllib.error.HTTPError as e:
        status = e.code
        try:
            body = e.read().decode("utf-8", "replace")[:200]
        except Exception:
            body = ""
    except Exception as e:
        body = f"{type(e).__name__}: {e}"[:200]
    ok = status == 200
    _log({"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "label": label,
          "outcome": "memory_cleared", "slot_erase_ok": ok,
          "http_status": status, "body": body})
    if not ok:
        print(f"WARNING: slot erase FAILED (http {status}): {body} - "
              f"the retry proceeds on a possibly warm KV cache",
              file=sys.stderr)
    return ok


def timed_call(messages, label, known_modules=None):
    """One timed request. Returns the response text.

    Raises LlmTimeout past ABORT_S, LlmHallucination on a tripwire.
    """
    from assemble import approx_tokens
    prompt = "\n".join(m["content"] for m in messages)
    body = json.dumps({"model": MODEL, "messages": messages,
                       "temperature": 0.2, "max_tokens": MAX_TOKENS}).encode()
    req = urllib.request.Request(API, data=body, method="POST",
                                 headers={"Content-Type": "application/json"})
    t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=ABORT_S) as r:
            payload = json.load(r)
    except Exception as e:
        elapsed = time.monotonic() - t0
        if elapsed >= ABORT_S * 0.95 or "timed out" in str(e).lower():
            _log({"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "label": label,
                  "elapsed_s": round(elapsed, 1), "outcome": "timeout",
                  "prompt_tokens": approx_tokens(prompt)})
            raise LlmTimeout(elapsed) from e
        raise
    elapsed = time.monotonic() - t0
    choice = payload["choices"][0]
    content = choice["message"]["content"]
    record = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "label": label,
              "elapsed_s": round(elapsed, 1),
              "prompt_tokens": approx_tokens(prompt),
              "resp_chars": len(content),
              "finish_reason": choice.get("finish_reason"),
              "outcome": "slow" if elapsed > WARN_S else "ok"}
    loop = _loop_line(content)
    if loop is not None:
        record["outcome"], record["detail"] = "loop", loop[:80]
        _log(record)
        raise LlmHallucination("loop", f"line repeated 6+ times: {loop[:80]}", elapsed)
    if choice.get("finish_reason") == "length":
        record["outcome"] = "truncated"
        _log(record)
        raise LlmHallucination("truncated", "hit the token cap", elapsed)
    invented = _invented_modules(content, known_modules)
    if invented:
        record["outcome"], record["detail"] = "invented", ",".join(invented)
        _log(record)
        raise LlmHallucination("invented", f"crate modules with no card: {invented}", elapsed)
    _log(record)
    return content
