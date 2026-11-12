"""Authenticated loopback ingress and a single event-driven Forge worker."""

from dataclasses import dataclass
from enum import Enum, auto
from http import HTTPStatus
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import logging
import math
import os
from pathlib import Path
import re
import secrets
import threading
import time
from urllib.parse import parse_qs, urlsplit
from uuid import UUID, uuid4

import psycopg2
import queue_store as store
from queue_store import Message, Rejected, Submit

PORT = 18083
DEFAULT_MODEL = "gpt-oss-20b"
MAX_BODY = 128 * 1024
LOG = logging.getLogger("forge-agent-runner")


class Route(Enum):
    SUBMIT = auto()
    STATUS = auto()
    EVENTS = auto()


@dataclass(frozen=True)
class Lookup:
    run_id: UUID
    after: int = 0
    wait: int = 0


def dispatch(route, command):
    if route is Route.SUBMIT and isinstance(command, Submit):
        return store.admit(command)
    if route is Route.STATUS and isinstance(command, Lookup):
        return store.get_run(command.run_id)
    if route is Route.EVENTS and isinstance(command, Lookup):
        if command.wait:
            with store.listen("forge_agent_events") as listener:
                deadline = time.monotonic() + command.wait
                while True:
                    result = store.get_events(command.run_id, command.after)
                    remaining = deadline - time.monotonic()
                    if result["events"] or result["status"] in store.TERMINAL or remaining <= 0:
                        return result
                    store.wait_for_signal(listener, remaining)
        return store.get_events(command.run_id, command.after)
    raise Rejected(400, "invalid_route_payload")


def submission(body, key):
    fields = {"model", "messages", "max_tokens", "temperature", "stream", "conversation_id"}
    if not isinstance(body, dict) or body.keys() - fields or body.get("stream", False) is not False:
        raise Rejected(400, "invalid_chat")
    messages = body.get("messages")
    if not isinstance(messages, list) or not 1 <= len(messages) <= 64:
        raise Rejected(400, "invalid_messages")
    for message in messages:
        if (not isinstance(message, dict) or message.keys() != {"role", "content"}
                or message["role"] not in ("system", "user", "assistant")
                or not isinstance(message["content"], str) or not message["content"].strip()
                or "\0" in message["content"]):
            raise Rejected(400, "invalid_messages")
    if (not any(m["role"] == "user" for m in messages)
            or sum(len(m["content"].encode()) for m in messages) > 65536):
        raise Rejected(400, "invalid_messages")
    model = body.get("model", DEFAULT_MODEL)
    max_tokens = body.get("max_tokens", 256)
    temperature = body.get("temperature", 0.0)
    if (not isinstance(model, str) or model not in store.MODELS
            or type(max_tokens) is not int or not 1 <= max_tokens <= 2048
            or type(temperature) not in (int, float) or not math.isfinite(temperature)
            or not 0 <= temperature <= 2):
        raise Rejected(400, "invalid_inference_settings")
    conversation = body.get("conversation_id")
    if conversation is not None:
        conversation = UUID(conversation)
    if key is not None and not re.fullmatch(r"[A-Za-z0-9_.:-]{1,96}", key):
        raise Rejected(400, "invalid_request_id")
    return Submit(model, tuple(Message(**m) for m in messages), max_tokens, float(temperature),
                  conversation, key or str(uuid4()))


class Inference:
    def __init__(self, token):
        self.token = token

    def request(self, method, path, body=None, timeout=185):
        connection = http.client.HTTPConnection("127.0.0.1", 18082, timeout=timeout)
        try:
            connection.request(method, path, json.dumps(body) if body is not None else None,
                               {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"})
            response = connection.getresponse()
            if response.status != 200:
                raise Rejected(502, {409: "forge_busy", 504: "forge_timeout"}.get(response.status, "forge_unavailable"))
            raw = response.read(MAX_BODY + 1)
            if len(raw) > MAX_BODY:
                raise Rejected(502, "forge_response_too_large")
            return json.loads(raw)
        except TimeoutError as error:
            raise Rejected(504, "forge_timeout") from error
        except (OSError, http.client.HTTPException) as error:
            raise Rejected(502, "forge_unavailable") from error
        finally:
            connection.close()

    def complete(self, command):
        runtime = self.request("GET", "/v1/runtime", timeout=3)
        if runtime.get("state") != "ready" or runtime.get("model") != command.model:
            raise Rejected(409, "model_not_ready")
        response = self.request("POST", "/v1/chat/completions", command.payload())
        choice = response["choices"][0]
        if choice.get("finish_reason") == "length":
            raise Rejected(422, "output_budget_exhausted")
        message = choice["message"]
        content = message.get("content")
        if (response.get("model") != command.model or choice.get("finish_reason") != "stop"
                or message.get("role") != "assistant" or not isinstance(content, str)
                or not content.strip() or "\0" in content or len(content.encode()) > 65536):
            raise Rejected(502, "invalid_forge_reply")
        usage = response.get("usage")
        return content, usage if isinstance(usage, dict) else {}

    def active_model(self):
        runtime = self.request("GET", "/v1/runtime", timeout=2)
        model = runtime.get("model")
        return model if runtime.get("state") == "ready" and isinstance(model, str) and model in store.MODELS else DEFAULT_MODEL


def worker(inference, ready):
    while True:
        try:
            with store.listen("forge_agent_jobs") as listener, listener.cursor() as cursor:
                cursor.execute("SELECT pg_try_advisory_lock(20261112, 1)")
                if not cursor.fetchone()[0]:
                    raise RuntimeError("another runner owns the worker lease")
                store.recover()
                ready.set()
                while True:
                    job = store.claim()
                    if job is None:
                        store.wait_for_signal(listener, 15)
                        continue
                    run_id, command = job
                    try:
                        answer, usage = inference.complete(command)
                    except Rejected as error:
                        store.finish(run_id, error=error.code)
                    except (KeyError, IndexError, TypeError, ValueError, AttributeError):
                        store.finish(run_id, error="invalid_forge_reply")
                    else:
                        store.finish(run_id, answer=answer, usage=usage)
        except (psycopg2.Error, OSError, RuntimeError) as error:
            ready.clear()
            LOG.warning("Worker reconnecting (%s)", type(error).__name__)
            time.sleep(2)


def encode(value):
    if isinstance(value, UUID):
        return str(value)
    return value.isoformat()


class Handler(BaseHTTPRequestHandler):
    server_version = "ForgeAgentRunner/1"

    def log_message(self, format_string, *args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def do_GET(self):
        self.handle_request()

    def do_POST(self):
        self.handle_request()

    def respond(self, status, body):
        payload = json.dumps(body, default=encode).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def handle_request(self):
        try:
            authorization = self.headers.get("Authorization", "").encode("latin1")
            if not secrets.compare_digest(authorization, self.server.authorization):
                raise Rejected(401, "unauthorized")
            status, result = self.route()
        except Rejected as error:
            status, result = error.status, {"error": error.code}
        except (ValueError, OverflowError, AttributeError):
            status, result = 400, {"error": "invalid_request"}
        except psycopg2.Error:
            status, result = 503, {"error": "database_unavailable"}
        except TimeoutError:
            status, result = 408, {"error": "request_timeout"}
        try:
            self.respond(status, result)
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            pass

    def route(self):
        parsed = urlsplit(self.path)
        if self.command == "GET" and parsed.path == "/health" and not parsed.query:
            status = HTTPStatus.OK if self.server.ready.is_set() else HTTPStatus.SERVICE_UNAVAILABLE
            return status, {"worker_ready": self.server.ready.is_set()}
        if self.command == "POST" and parsed.path in ("/v1/runs", "/v1/chat/completions") and not parsed.query:
            if self.headers.get_content_type() != "application/json":
                raise Rejected(415, "json_required")
            size = int(self.headers.get("Content-Length", "0"))
            if not 1 <= size <= MAX_BODY or self.headers.get("Transfer-Encoding"):
                raise Rejected(413, "invalid_body_size")
            body = json.loads(self.rfile.read(size))
            legacy = parsed.path == "/v1/chat/completions"
            if legacy and isinstance(body, dict) and "model" not in body:
                try:
                    body["model"] = self.server.inference.active_model()
                except (Rejected, ValueError, AttributeError):
                    body["model"] = DEFAULT_MODEL
            command = submission(body, self.headers.get("Idempotency-Key"))
            run = dispatch(Route.SUBMIT, command)
            if legacy:
                return self.await_reply(run["run_id"])
            return HTTPStatus.ACCEPTED, run
        match = re.fullmatch(r"/v1/runs/([0-9a-fA-F-]{36})(/events)?", parsed.path)
        if self.command == "GET" and match:
            run_id = UUID(match[1])
            if match[2]:
                query = parse_qs(parsed.query, keep_blank_values=True, max_num_fields=2)
                if query.keys() - {"after", "wait"} or any(len(values) != 1 for values in query.values()):
                    raise Rejected(400, "invalid_event_cursor")
                after, wait = int(query.get("after", ["0"])[0]), int(query.get("wait", ["0"])[0])
                if not 0 <= after <= 2**63 - 1 or not 0 <= wait <= 20:
                    raise Rejected(400, "invalid_event_cursor")
                return HTTPStatus.OK, dispatch(Route.EVENTS, Lookup(run_id, after, wait))
            if not parsed.query:
                return HTTPStatus.OK, dispatch(Route.STATUS, Lookup(run_id))
        raise Rejected(404, "route_not_found")

    def await_reply(self, run_id):
        deadline = time.monotonic() + 13
        with store.listen("forge_agent_events") as listener:
            while True:
                run = dispatch(Route.STATUS, Lookup(run_id))
                if run["status"] == "completed":
                    return HTTPStatus.OK, {"run_id": run_id, "model": run["model"], "choices": [
                        {"message": {"role": "assistant", "content": run["answer"]}, "finish_reason": "stop"}
                    ]}
                if run["status"] in store.TERMINAL:
                    return HTTPStatus.BAD_GATEWAY, {"run_id": run_id, "error": run["status"]}
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return HTTPStatus.GATEWAY_TIMEOUT, {"run_id": run_id, "error": "reply_pending"}
                store.wait_for_signal(listener, remaining)


class Server(ThreadingHTTPServer):
    def __init__(self, token, ready, inference):
        self.authorization = f"Bearer {token}".encode()
        self.ready = ready
        self.inference = inference
        self.slots = threading.BoundedSemaphore(24)
        super().__init__(("127.0.0.1", PORT), Handler)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


def main():
    logging.basicConfig(level=logging.INFO)
    credentials = Path(os.environ["CREDENTIALS_DIRECTORY"])
    inbound = (credentials / "runner.token").read_text().strip()
    outbound = (credentials / "forge.token").read_text().strip()
    if not all(re.fullmatch(r"[a-fA-F0-9]{32,128}", token) for token in (inbound, outbound)):
        raise ValueError("Expected two hexadecimal bearer credentials")
    ready = threading.Event()
    inference = Inference(outbound)
    with Server(inbound, ready, inference) as server:
        threading.Thread(target=worker, args=(inference, ready), daemon=True).start()
        LOG.info("Listening on loopback port %d", PORT)
        server.serve_forever()


if __name__ == "__main__":
    main()
