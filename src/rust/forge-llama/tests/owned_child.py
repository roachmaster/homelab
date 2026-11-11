#!/usr/bin/env python3
"""Exercise lifecycle against a test child without requiring a GPU."""
import argparse
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
import json, pathlib, signal, sys

def value(flag):
    return sys.argv[sys.argv.index(flag) + 1]

model = pathlib.Path(value("--model"))
alias = value("--alias")
if alias == "bad":
    sys.exit(3)

def stop(_signal, _frame):
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
        assert self.path == "/v1/chat/completions"
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert request["model"] == alias and request["stream"] is False
        self.send({"id": "test", "object": "chat.completion", "created": 0, "model": alias,
                   "choices": [{"index": 0, "finish_reason": "stop",
                                "message": {"role": "assistant", "content": "42"}}]})

HTTPServer(("127.0.0.1", int(value("--port"))), Handler).serve_forever()
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        backend = directory / "test-llama"
        backend.write_text(BACKEND)
        backend.chmod(0o700)
        token = "ab" * 32
        secret = directory / "token"
        secret.touch(mode=0o600)
        secret.write_text(token + "\n")
        models = {name: directory / (name + ".gguf") for name in ("a", "b", "bad")}
        for model in models.values():
            model.write_bytes(b"GGUF")
        with socket.socket() as outer, socket.socket() as inner:
            outer.bind(("127.0.0.1", 0))
            inner.bind(("127.0.0.1", 0))
            port, backend_port = outer.getsockname()[1], inner.getsockname()[1]
        config = directory / "config.json"
        config.write_text(json.dumps({
            "server": str(backend), "backend_port": backend_port, "device": "SYCL0",
            "gpu_lock": str(directory / "gpu.lock"), "token_file": str(secret),
            "models": {name: str(path) for name, path in models.items()},
            "context_size": 4096, "startup_seconds": 3, "prompt_seconds": 3,
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
                assert response.status == expected, body
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
                assert request("PUT", runtime, {"model": "a"})["model"] == "a"
                reply = request("POST", "/v1/chat/completions",
                                {"messages": [{"role": "user", "content": "6 times 7"}]})
                assert reply["choices"][0]["message"]["content"] == "42"
                assert request("PUT", runtime, {"model": "b"})["model"] == "b"
                assert models["a"].with_suffix(".stopped").read_text() == "SIGTERM"
                assert request("PUT", runtime, {"model": "bad"}, 502)["error"] == "startup_failed"
                assert models["b"].with_suffix(".stopped").read_text() == "SIGTERM"
                assert request("GET", runtime)["state"] == "stopped"
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
    print("PASS: chat, model switch, failed replacement, and graceful child shutdown")


if __name__ == "__main__":
    main()
