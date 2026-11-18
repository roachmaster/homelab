#!/usr/bin/env python3
"""Exercise lifecycle against a test child without requiring a GPU."""
import argparse
from fcntl import LOCK_EX, LOCK_NB, flock
from http.client import HTTPConnection
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

BACKEND = '''#!/usr/bin/python3
from http.server import BaseHTTPRequestHandler, HTTPServer
import fcntl, json, os, pathlib, signal, sys, time

def value(flag):
    return sys.argv[sys.argv.index(flag) + 1]

model = pathlib.Path(value("--model"))
alias = value("--alias")

def assert_gpu_reserved():
    with model.with_name("gpu.lock").open("r+b") as contender:
        try:
            fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
    raise AssertionError("GPU reservation released while child was active")

assert_gpu_reserved()
if alias == "bad":
    sys.exit(3)
if alias == "gpt-oss-120b":
    for flag, expected in {"--ctx-size": "4096", "--moe-cache-mib": "4096", "--load-mode": "mmap",
                           "--batch-size": "8", "--ubatch-size": "1", "--reasoning-effort": "low"}.items():
        assert value(flag) == expected
    assert {"--cpu-moe", "--no-repack", "--no-context-shift"}.issubset(sys.argv)
    assert os.environ["ZES_ENABLE_SYSMAN"] == "1"
else:
    assert value("--ctx-size") == "4096"
    assert value("--batch-size") == "128"
    assert "--cpu-moe" not in sys.argv

def stop(_signal, _frame):
    assert_gpu_reserved()
    model.with_suffix(".stopped").write_text("SIGTERM")
    sys.exit(0)

signal.signal(signal.SIGTERM, stop)

class Handler(BaseHTTPRequestHandler):
    def send(self, data):
        body = json.dumps(data).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        assert self.path == "/health"
        self.send({"status": "ok"})

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/v1/chat/completions/input_tokens":
            assert alias == "gpt-oss-120b" and "mail_profile" not in request
            self.send({"input_tokens": 12 + 5 * len(request["messages"])})
            return
        assert self.path == "/v1/chat/completions"
        assert request["model"] == alias and request["stream"] is False
        assert request["cache_prompt"] == (alias == "gpt-oss-120b")
        assert "mail_profile" not in request
        if request["messages"][-1]["content"] == "cancel this completion":
            model.with_suffix(".running").touch()
            time.sleep(30)
        self.send({"id": "test", "object": "chat.completion", "created": 0, "model": alias,
                   "usage": {"prompt_tokens": 12 + 5 * len(request["messages"]),
                             "completion_tokens": 1, "total_tokens": 13 + 5 * len(request["messages"])},
                   "choices": [{"index": 0, "finish_reason": "stop",
                                "message": {"role": "assistant", "content": "42",
                                            "reasoning_content": "reasoning"}}]})

HTTPServer(("127.0.0.1", int(value("--port"))), Handler).serve_forever()
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        gpu_lock = directory / "gpu.lock"
        backend = directory / "test-llama"
        backend.write_text(BACKEND)
        backend.chmod(0o700)
        token = "ab" * 32
        secret = directory / "token"
        secret.touch(mode=0o600)
        secret.write_text(token + "\n")
        mail_model, profile = "gpt-oss-120b", "gpt-oss-120b-harmony-v1"
        models = {name: directory / (name + ".gguf") for name in ("a", "b", "bad", mail_model)}
        for model in models.values():
            model.write_bytes(b"GGUF")
        with socket.socket() as outer, socket.socket() as inner:
            outer.bind(("127.0.0.1", 0))
            inner.bind(("127.0.0.1", 0))
            port, backend_port = outer.getsockname()[1], inner.getsockname()[1]
        config = directory / "config.json"
        config.write_text(json.dumps({
            "server": str(backend), "backend_port": backend_port, "device": "SYCL0",
            "gpu_lock": str(gpu_lock), "token_file": str(secret),
            "models": {name: str(path) for name, path in models.items()},
            "context_size": 4096, "startup_seconds": 3, "prompt_seconds": 3,
            "mail_expert_paging": {"profile_id": profile, "context_size": 4096,
                                   "generation_tokens": 1024, "margin_tokens": 16, "prompt_seconds": 6},
        }))

        def request(method, path, data=None, expected=200):
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}{path}", method=method,
                headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
                data=None if data is None else json.dumps(data).encode(),
            )
            try:
                response = urllib.request.urlopen(req, timeout=10)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                body = json.load(response)
                assert response.status in ((expected,) if isinstance(expected, int) else expected), body
                return body

        runtime = "/v1/runtime"
        with (directory / "log").open("w+") as log:
            service = subprocess.Popen([str(args.binary.resolve()), str(config), str(port)],
                                       stdout=log, stderr=log)
            try:
                for attempt in range(60):
                    try:
                        assert request("GET", runtime)["state"] == "stopped"
                        break
                    except urllib.error.URLError:
                        if service.poll() is not None or attempt == 59:
                            raise
                        time.sleep(0.05)
                assert not gpu_lock.exists()
                with gpu_lock.open("a") as other_process:
                    flock(other_process, LOCK_EX | LOCK_NB)
                    assert request("PUT", runtime, {"model": "a"}, 409)["error"] == "runtime_busy"
                    assert request("GET", runtime)["state"] == "stopped"
                assert request("PUT", runtime, {"model": "a"})["model"] == "a"
                reply = request("POST", "/v1/chat/completions",
                                {"messages": [{"role": "user", "content": "6 times 7"}]})
                assert reply["choices"][0]["message"]["content"] == "42"
                assert request("PUT", runtime, {"model": "b"})["model"] == "b"
                assert models["a"].with_suffix(".stopped").read_text() == "SIGTERM"
                assert request("PUT", runtime, {"model": "bad"}, 502)["error"] == "startup_failed"
                assert models["b"].with_suffix(".stopped").read_text() == "SIGTERM"
                assert request("GET", runtime)["state"] == "stopped"
                with gpu_lock.open("r+b") as other_process:
                    flock(other_process, LOCK_EX | LOCK_NB)
                assert request("PUT", runtime, {"model": mail_model})["model"] == mail_model
                completion_path = "/v1/chat/completions"
                messages = [{"role": "user", "content": "Remember my first question"},
                            {"role": "assistant", "content": "Yes"},
                            {"role": "user", "content": "What is 6 times 7?"}]
                prompt = {"model": mail_model, "messages": messages}
                assert request("POST", completion_path + "/input_tokens", prompt) == {"input_tokens": 27}
                metadata = {"profile_id": profile, "prompt_tokens": 27}
                completion = {**prompt, "mail_profile": metadata, "max_tokens": 1024}
                cases = [prompt, {**completion, "max_tokens": 1025},
                         {**completion, "mail_profile": {**metadata, "profile_id": "old"}},
                         {**completion, "mail_profile": {**metadata, "prompt_tokens": 28}},
                         {**completion, "mail_profile": {**metadata, "prompt_tokens": 3057}}]
                for invalid in cases:
                    reply = request("POST", completion_path, invalid, 422)
                    assert reply["error"] == "mail_profile_mismatch"
                reply = request("POST", completion_path, completion)
                assert reply["choices"][0]["message"] == {"role": "assistant", "content": "42"}

                messages[-1]["content"] = "cancel this completion"
                wire = HTTPConnection("127.0.0.1", port, timeout=2)
                wire.request("POST", completion_path, json.dumps(completion),
                             {"Authorization": "Bearer " + token, "Content-Type": "application/json"})
                for _ in range(100):
                    if models[mail_model].with_suffix(".running").exists():
                        break
                    time.sleep(0.02)
                else:
                    raise AssertionError("test completion never started")
                wire.sock.shutdown(socket.SHUT_RDWR)
                wire.close()
                for _ in range(100):
                    if request("GET", runtime, expected=(200, 409)).get("state") == "stopped":
                        break
                    time.sleep(0.02)
                else:
                    raise AssertionError("disconnected Mail inference did not stop")
                assert models[mail_model].with_suffix(".stopped").read_text() == "SIGTERM"
                assert request("PUT", runtime, {"model": "a"})["model"] == "a"
                models["a"].with_suffix(".stopped").unlink()
            finally:
                service.terminate()
                try:
                    assert service.wait(timeout=15) == 0
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.wait()
                    raise
            assert models["a"].with_suffix(".stopped").read_text() == "SIGTERM"
    print("PASS: GPU reservation, chat, model switch, Mail token budget, disconnect cancellation, and child shutdown")


if __name__ == "__main__":
    main()
