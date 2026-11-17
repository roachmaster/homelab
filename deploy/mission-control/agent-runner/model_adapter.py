"""Bounded calls to the model adapter on the current host's loopback."""

import http.client
import json
import socket
import threading


MAX_BODY = 128 * 1024
# The adapter supports up to 180 seconds for startup and reaps a replaced
# child before loading and a failed child before returning an error.
MODEL_LOAD_SECONDS = 200
MAIL_MODEL = "gpt-oss-120b"
# The Mail profile allows 1800 seconds for inference plus token counting and child cleanup.
MAIL_RESPONSE_SECONDS = 1820


class ModelError(Exception):
    def __init__(self, status, code):
        self.status = status
        self.code = code
        super().__init__(code)


class ModelAdapter:
    failure = ModelError

    def __init__(self, token, port):
        self.token = token
        self.port = port
        self._lock = threading.Lock()
        self._socket = None

    def request(self, method, path, body=None, timeout=185, *, stop=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout)
        wire = None
        try:
            connection.connect()
            wire = connection.sock
            with self._lock:
                if stop is not None and stop.is_set():
                    raise self.failure(409, "lease_lost")
                self._socket = wire
            connection.request(method, path, json.dumps(body) if body is not None else None,
                               {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"})
            response = connection.getresponse()
            if response.status != 200:
                if response.status == 422 and isinstance(body, dict) and body.get("model") == MAIL_MODEL:
                    raise self.failure(422, "mail_profile_mismatch")
                code = {409: "forge_busy", 504: "forge_timeout"}.get(response.status, "forge_unavailable")
                raise self.failure(502, code)
            raw = response.read(MAX_BODY + 1)
            if len(raw) > MAX_BODY:
                raise self.failure(502, "forge_response_too_large")
            return json.loads(raw)
        except TimeoutError as error:
            raise self.failure(504, "forge_timeout") from error
        except (OSError, http.client.HTTPException) as error:
            raise self.failure(502, "forge_unavailable") from error
        finally:
            with self._lock:
                if self._socket is wire:
                    self._socket = None
            connection.close()

    def interrupt(self):
        with self._lock:
            if self._socket is not None:
                try:
                    self._socket.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def ready_model(self):
        runtime = self.request("GET", "/v1/runtime", timeout=2)
        model = runtime.get("model")
        return model if runtime.get("state") == "ready" and isinstance(model, str) else None

    def load_model(self, model):
        runtime = self.request("PUT", "/v1/runtime", {"model": model}, timeout=MODEL_LOAD_SECONDS)
        if not isinstance(runtime, dict) or runtime.get("state") != "ready" or runtime.get("model") != model:
            raise self.failure(502, "invalid_forge_runtime")

    def generate(self, model, request, *, allow_partial=False, stop=None):
        options = {} if stop is None else {"stop": stop}
        runtime = self.request("GET", "/v1/runtime", timeout=3, **options)
        if runtime.get("state") != "ready" or runtime.get("model") != model:
            raise self.failure(409, "model_not_ready")
        if model == MAIL_MODEL:
            options["timeout"] = MAIL_RESPONSE_SECONDS
        response = self.request("POST", "/v1/chat/completions", request, **options)
        choice = response["choices"][0]
        finish_reason = choice.get("finish_reason")
        message = choice["message"]
        content = message.get("content")
        if finish_reason == "length" and (
            not allow_partial or not isinstance(content, str) or not content.strip()
        ):
            raise self.failure(422, "output_budget_exhausted")
        if (response.get("model") != model or finish_reason not in {"stop", "length"}
                or message.get("role") != "assistant" or not isinstance(content, str)
                or not content.strip() or "\0" in content or len(content.encode()) > 65536):
            raise self.failure(502, "invalid_forge_reply")
        usage = response.get("usage")
        return content, usage if isinstance(usage, dict) else {}, finish_reason
