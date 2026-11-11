#!/usr/bin/env python3
"""Exercise an idle Forge adapter and return it to the stopped runtime state."""
import argparse
import json
from pathlib import Path
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--port", type=int, default=18080)
    args = parser.parse_args()
    token = args.token_file.read_text().strip()
    base = f"http://127.0.0.1:{args.port}"

    def request(method, path, body=None, expected=200, authorized=True):
        headers = {"Authorization": f"Bearer {token}"} if authorized else {}
        if body is not None:
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(
            base + path, method=method, headers=headers,
            data=None if body is None else json.dumps(body).encode(),
        )
        try:
            response = urllib.request.urlopen(req, timeout=200)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            value = json.load(response)
            assert response.status == expected, (method, path, response.status, value)
            return value

    runtime = "/v1/runtime"
    chat = "/v1/chat/completions"
    models = ("qwen2.5-coder-7b", "gpt-oss-20b")
    assert request("GET", runtime, expected=401, authorized=False)["error"] == "unauthorized"
    assert request("GET", runtime)["state"] == "stopped", "Refusing to replace an existing model"
    assert set(models) <= {row["id"] for row in request("GET", "/v1/models")["data"]}
    prompt = {"messages": [{"role": "user", "content": "What is 6 times 7? Reply with just the number."}],
              "max_tokens": 256, "temperature": 0}
    assert request("POST", chat, prompt, expected=503)["error"] == "runtime_stopped"
    answers = []
    try:
        for model in models:
            assert request("PUT", runtime, {"model": model}) == {"state": "ready", "model": model}
            print(f"{model} ready", flush=True)
            assert request("PUT", runtime, {"model": model})["model"] == model
            assert request("PUT", runtime, {"model": "/tmp/other.gguf"}, expected=404)["error"] == "unknown_model"
            assert request("GET", runtime)["model"] == model
            wrong = next(name for name in models if name != model)
            assert request("POST", chat, {**prompt, "model": wrong}, expected=409)["error"] == "model_not_loaded"
            result = request("POST", chat, {**prompt, "model": model})
            assert result["model"] == model
            answer = result["choices"][0]["message"]["content"]
            assert answer is not None and answer.strip() in {"42", "42.", "**42**"}, (model, answer)
            assert result["usage"]["completion_tokens"] > 0
            answers.append({"model": model, "answer": answer.strip(), "usage": result["usage"]})
            print(f"{model} prompt passed", flush=True)
    finally:
        assert request("DELETE", runtime) == {"state": "stopped", "model": None}
    assert request("DELETE", runtime)["state"] == "stopped"
    print(json.dumps({"passed": True, "answers": answers, "final_state": "stopped"}))


if __name__ == "__main__":
    main()
