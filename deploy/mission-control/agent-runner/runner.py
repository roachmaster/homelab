"""Authenticated loopback ingress and durable Forge run completion."""

from dataclasses import dataclass, replace
from contextlib import ExitStack, contextmanager
from enum import Enum, auto
from http import HTTPStatus
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
import agent_tools
import chat_history_api as history_api
import chat_history_store as history_store
import chat_run_api as run_api
from file_context import parse_attachment
import mail_api
import jenkins_api
import jenkins_config
from model_adapter import MAX_BODY, ModelAdapter
import queue_store as store
from queue_store import Message, Rejected, Submit
import usage_store
import work_api
import work_config

try:
    import usage_api
except ModuleNotFoundError:
    usage_api = None


class _UnavailableUsageApi:
    """Fallback router when usage_api is not installed so ide.usage.* never hits GPU/Chat paths."""

    MAX_REQUEST_BYTES = 16 * 1024
    _KNOWN = frozenset({"ide.usage.overview", "ide.usage.detail", "ide.usage.refresh"})

    @staticmethod
    def handles(headers):
        return any(value.startswith("ide.usage.") for value in headers.get_all("Forge-Destination", []))

    @classmethod
    def destination(cls, headers):
        value = history_api.dispatch_destination(headers)
        if value in cls._KNOWN:
            return value
        raise Rejected(404, "not_found")

    @staticmethod
    def decode(_route, correlation, body):
        if (not isinstance(body, dict)
                or type(body.get("request_id")) is not str
                or body.get("request_id") != correlation):
            raise Rejected(400, "invalid_request")
        return body

    @staticmethod
    def failed(correlation, error):
        if correlation is None:
            return error.status, {"error": "invalid_request"}
        if error.status in {400, 413, 415}:
            code = "invalid_request"
        elif error.status in {401, 403}:
            code = "denied"
        elif error.status == 404:
            code = "not_found"
        else:
            code = "unavailable"
        status = 503 if code == "unavailable" else error.status
        return status, history_api.envelope("usage_failed", correlation, code=code)

    @staticmethod
    def dispatch(_route, _correlation, _command, _service):
        raise Rejected(503, "unavailable")

PORT = 18083
DEFAULT_MODEL = "gpt-oss-20b"
WORKSPACE_PATH = "/v1/workspace"
TOOL_RUN_PATH = "/v1/agent/runs"
MAX_WORKSPACE_BODY = 2 * 1024 * 1024
HISTORY_EXPIRY_INTERVAL_SECONDS = 60
RESULT_SCAN_SECONDS = 2
RESULT_BATCH = 20
LOG = logging.getLogger("forge-agent-runner")


class Route(Enum):
    SUBMIT = auto()
    STATUS = auto()
    EVENTS = auto()
    WORKSPACE = auto()


@dataclass(frozen=True)
class Lookup:
    run_id: UUID
    after: int = 0
    wait: int = 0


@dataclass(frozen=True)
class RestoreWorkspace:
    tab: str
    history_enabled: bool = False
    models: bool = False
    preserve_chat: bool = False


@dataclass(frozen=True)
class SaveWorkspace:
    tab: str
    lease: str
    revision: int
    snapshot: dict
    preserve_chat: bool = False


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
    if route is Route.WORKSPACE:
        if isinstance(command, RestoreWorkspace):
            if command.preserve_chat:
                return store.restore_workspace(command.tab, preserve_chat=True)
            if command.models:
                return store.restore_workspace(command.tab, history_enabled=True, models=True)
            if command.history_enabled:
                return store.restore_workspace(command.tab, history_enabled=True)
            return store.restore_workspace(command.tab)
        if isinstance(command, SaveWorkspace):
            if command.preserve_chat:
                return store.save_workspace(command.tab, command.lease, command.revision, command.snapshot,
                                            preserve_chat=True)
            return store.save_workspace(command.tab, command.lease, command.revision, command.snapshot)
    raise Rejected(400, "invalid_route_payload")


def workspace_command(body):
    if not isinstance(body, dict):
        raise Rejected(400, "invalid_workspace_request")
    operation, tab = body.get("operation"), body.get("tab")
    if not isinstance(tab, str) or not re.fullmatch(r"[0-9a-f]{32}", tab):
        raise Rejected(400, "invalid_workspace_tab")
    if operation == "restore":
        if body.keys() == {"operation", "tab"}:
            return RestoreWorkspace(tab)
        if body.keys() == {"operation", "tab", "preserve_chat"} and body["preserve_chat"] is True:
            return RestoreWorkspace(tab, preserve_chat=True)
        if body.keys() == {"operation", "tab", "history"} and body["history"] is True:
            return RestoreWorkspace(tab, history_enabled=True)
        if body.keys() == {"operation", "tab", "history", "models"} and body["history"] is True and body["models"] is True:
            return RestoreWorkspace(tab, history_enabled=True, models=True)
    save_keys = {"operation", "tab", "lease", "revision", "snapshot"}
    preserve_chat = body.keys() == save_keys | {"preserve_chat"} and body["preserve_chat"] is True
    if operation != "save" or (body.keys() != save_keys and not preserve_chat):
        raise Rejected(400, "invalid_workspace_request")
    lease, revision, snapshot = body["lease"], body["revision"], body["snapshot"]
    if (not isinstance(lease, str) or not re.fullmatch(r"[0-9a-f]{64}", lease)
            or type(revision) is not int or not 1 <= revision <= 2**32 - 1):
        raise Rejected(400, "invalid_workspace_request")
    if not store.valid_workspace(snapshot):
        raise Rejected(400, "invalid_workspace_snapshot")
    return SaveWorkspace(tab, lease, revision, snapshot, preserve_chat=preserve_chat)


def unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def workspace_json(body):
    return json.dumps(body, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")


def workspace_json_safe(body):
    # PostgreSQL jsonb rejects U+0000, including in object keys.
    pending = [iter((body,))]
    while pending:
        for value in pending[-1]:
            if isinstance(value, str) and "\0" in value:
                return False
            if isinstance(value, list):
                pending.append(iter(value))
                break
            if isinstance(value, dict):
                pending.extend((iter(value.values()), iter(value.keys())))
                break
        else:
            pending.pop()
    return True


def submission(body, key, *, allow_partial=False):
    fields = {"model", "messages", "max_tokens", "temperature", "stream", "conversation_id"}
    if allow_partial:
        fields.update(("chat_id", "expected_revision", "attachment"))
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
    linked_chat = history_api.parse_chat_id(body["chat_id"]) if "chat_id" in body else None
    expected_revision = None
    if "expected_revision" in body:
        if linked_chat is None:
            raise Rejected(400, "invalid_chat_id")
        expected_revision = history_api.parse_revision(body["expected_revision"])
    if linked_chat is not None and conversation is not None:
        raise Rejected(400, "invalid_chat_id")
    attachment = None
    if "attachment" in body:
        if linked_chat is None:
            raise Rejected(400, "invalid_chat_id")
        attachment = parse_attachment(body["attachment"])
    return Submit(model, tuple(Message(**m) for m in messages), max_tokens, float(temperature),
                  conversation, key or str(uuid4()), allow_partial=allow_partial,
                  chat_id=linked_chat, expected_revision=expected_revision, attachment=attachment)


class Inference(ModelAdapter):
    failure = Rejected

    def __init__(self, token):
        super().__init__(token, 18082)

    def complete(self, command):
        return self.generate(command.model, command.payload(), allow_partial=command.allow_partial)

    def active_model(self):
        model = self.ready_model()
        return model if model in store.MODELS else DEFAULT_MODEL


def tool_submission(body, key, repositories):
    if not isinstance(body, dict) or not {"repository", "revision"} <= body.keys():
        raise Rejected(400, "invalid_tool_run")
    repository, revision = body["repository"], body["revision"]
    if not isinstance(repository, str) or repository not in repositories:
        raise Rejected(404, "repository_not_found")
    if not isinstance(revision, str) or not agent_tools.OBJECT_ID.fullmatch(revision):
        raise Rejected(400, "invalid_revision")
    settings = {field: value for field, value in body.items() if field not in {"repository", "revision"}}
    if "conversation_id" in settings or key is None:
        raise Rejected(400, "invalid_tool_run")
    return replace(submission(settings, key), tool_repository=repository, tool_revision=revision)


def complete_run(inference, run_id, command, repositories):
    if command.tool_repository is None:
        return inference.complete(command)
    root = repositories.get(command.tool_repository)
    if root is None:
        raise Rejected(404, "repository_not_found")
    snapshot = agent_tools.Snapshot(root, command.tool_revision)
    return agent_tools.complete(
        lambda request: inference.generate(command.model, request), command.payload(), snapshot,
        lambda call: store.tool_started(run_id, call),
        lambda call, result: store.tool_finished(run_id, call, result),
    )


def worker(inference, ready, repositories=None, *, tools_only=False):
    repositories = {} if repositories is None else repositories
    while True:
        try:
            with store.listen("forge_agent_jobs") as listener, listener.cursor() as cursor:
                cursor.execute("SELECT pg_try_advisory_lock(20261112, 1)")
                if not cursor.fetchone()[0]:
                    raise RuntimeError("another runner owns the worker lease")
                store.recover(tools_only=tools_only)
                ready.set()
                while True:
                    job = store.claim(tools_only=tools_only)
                    if job is None:
                        store.wait_for_signal(listener, 15)
                        continue
                    run_id, command = job
                    try:
                        answer, usage, finish_reason = complete_run(inference, run_id, command, repositories)
                    except (Rejected, agent_tools.ToolError) as error:
                        store.finish(run_id, error=error.code)
                    except (KeyError, IndexError, TypeError, ValueError, AttributeError):
                        store.finish(run_id, error="invalid_forge_reply")
                    else:
                        store.finish(run_id, answer=answer, usage=usage, finish_reason=finish_reason)
        except (psycopg2.Error, OSError, RuntimeError) as error:
            ready.clear()
            LOG.warning("Worker reconnecting (%s)", type(error).__name__)
            time.sleep(2)


def expire_chat_history():
    while True:
        try:
            history_store.dao.expire_due()
        except (psycopg2.Error, OSError) as error:
            LOG.warning("Chat history expiry retrying (%s)", type(error).__name__)
        time.sleep(HISTORY_EXPIRY_INTERVAL_SECONDS)


def complete_dispatched_runs(ready):
    while True:
        try:
            with store.listen("forge_agent_results") as listener:
                while True:
                    completed = store.finalize_ready(RESULT_BATCH)
                    expired = store.expire_runs(RESULT_BATCH)
                    ready.set()
                    if max(completed, expired) < RESULT_BATCH:
                        store.wait_for_signal(listener, RESULT_SCAN_SECONDS)
        except (psycopg2.Error, OSError, RuntimeError) as error:
            ready.clear()
            LOG.warning("Dispatch completion reconnecting (%s)", type(error).__name__)
            time.sleep(RESULT_SCAN_SECONDS)


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
        if self.path == WORKSPACE_PATH:
            try:
                payload = workspace_json(body)
            except (TypeError, ValueError, RecursionError):
                payload = b""
            if not payload or len(payload) > MAX_WORKSPACE_BODY:
                status = HTTPStatus.SERVICE_UNAVAILABLE
                payload = b'{"error":"invalid_workspace_response"}'
        elif self.path == history_api.PATH:
            try:
                payload = json.dumps(body, ensure_ascii=False, allow_nan=False, default=encode,
                                     separators=(",", ":")).encode("utf-8")
            except (AttributeError, TypeError, ValueError, RecursionError):
                payload = b""
            if not payload or len(payload) > history_api.MAX_RESPONSE_BYTES:
                api = self.versioned_api()
                status, body = api.failed(
                    history_api.request_id(self.headers), Rejected(503, "unavailable")
                )
                payload = json.dumps(body).encode()
        else:
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
            if (not secrets.compare_digest(authorization, self.server.authorization)
                    or (self.path in (WORKSPACE_PATH, history_api.PATH, TOOL_RUN_PATH)
                        and len(self.headers.get_all("Authorization", [])) != 1)):
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
        if self.command == "POST" and self.path == WORKSPACE_PATH:
            command = workspace_command(self.read_workspace_body())
            return HTTPStatus.OK, dispatch(Route.WORKSPACE, command)
        if self.command == "POST" and self.path == history_api.PATH:
            return self.versioned_request()
        if self.command == "POST" and self.path == TOOL_RUN_PATH:
            if not self.server.tool_repositories:
                raise Rejected(404, "route_not_found")
            body = self.read_json_body(MAX_BODY, "invalid_tool_run")
            keys = self.headers.get_all("Idempotency-Key", [])
            if len(keys) != 1:
                raise Rejected(400, "invalid_request_id")
            if isinstance(body, dict):
                body.setdefault("model", self.server.pinned_model)
            command = tool_submission(body, keys[0], self.server.tool_repositories)
            if command.model not in self.server.configured_models:
                raise Rejected(400, "invalid_model")
            return HTTPStatus.ACCEPTED, dispatch(Route.SUBMIT, command)
        if self.command == "POST" and parsed.path in ("/v1/runs", "/v1/chat/completions") and not parsed.query:
            if self.headers.get_content_type() != "application/json":
                raise Rejected(415, "json_required")
            size = int(self.headers.get("Content-Length", "0"))
            if not 1 <= size <= MAX_BODY or self.headers.get("Transfer-Encoding"):
                raise Rejected(413, "invalid_body_size")
            body = json.loads(self.rfile.read(size))
            legacy = parsed.path == "/v1/chat/completions"
            distributed = getattr(self.server, "distributed", False)
            if isinstance(body, dict) and "model" not in body:
                if distributed:
                    body["model"] = self.server.pinned_model
                elif legacy:
                    try:
                        body["model"] = self.server.inference.active_model()
                    except (Rejected, ValueError, AttributeError):
                        body["model"] = DEFAULT_MODEL
            command = submission(body, self.headers.get("Idempotency-Key"), allow_partial=legacy)
            run = store.admit(command, distributed=True) if distributed else dispatch(Route.SUBMIT, command)
            if legacy:
                return self.await_reply(run["run_id"], command.chat_id is not None)
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

    def versioned_api(self):
        if jenkins_api.handles(self.headers):
            return jenkins_api
        if work_api.handles(self.headers):
            return work_api
        u_api = usage_api if usage_api is not None else _UnavailableUsageApi
        if u_api.handles(self.headers):
            return u_api
        if mail_api.handles(self.headers):
            return mail_api
        return run_api if run_api.handles(self.headers) else history_api

    def versioned_request(self):
        api = self.versioned_api()
        u_api = usage_api if usage_api is not None else _UnavailableUsageApi
        correlation = history_api.request_id(self.headers)
        try:
            if correlation is None:
                raise Rejected(400, "invalid_request")
            route = api.destination(self.headers)
            limit = (
                jenkins_api.MAX_REQUEST_BYTES if api is jenkins_api
                else work_api.MAX_REQUEST_BYTES if api is work_api
                else getattr(u_api, "MAX_REQUEST_BYTES", history_api.MAX_REQUEST_BYTES) if api is u_api
                else mail_api.MAX_REQUEST_BYTES if api is mail_api
                else history_api.MAX_REQUEST_BYTES
            )
            body = self.read_json_body(limit, "invalid_request")
            command = api.decode(route, correlation, body)
            if api is jenkins_api:
                jenkins_service = getattr(self.server, "jenkins_service", None)
                if jenkins_service is None:
                    raise Rejected(503, "unavailable")
                return api.dispatch(route, correlation, command, jenkins_service)
            if api is work_api:
                work_service = getattr(self.server, "work_service", None)
                if work_service is None:
                    raise Rejected(503, "unavailable")
                return api.delivery_response(api.dispatch(route, correlation, command, work_service,
                                                                    delivery_version=api.delivery_version(self.headers)),
                                             api.delivery_version(self.headers))
            if api is u_api:
                usage_service = getattr(self.server, "usage_service", None)
                if usage_service is None:
                    raise Rejected(503, "unavailable")
                return api.dispatch(route, correlation, command, usage_service)
            if api in (run_api, mail_api) and not getattr(self.server, "distributed", False):
                raise Rejected(503, "unavailable")
            if api is run_api:
                return api.dispatch(route, correlation, command, self.server.pinned_model, self.server.configured_models)
            return api.dispatch(route, correlation, command)
        except Rejected as error:
            return api.failed(correlation, error)
        except psycopg2.Error:
            return api.failed(correlation, Rejected(503, "unavailable"))
        except TimeoutError:
            return api.failed(correlation, Rejected(408, "unavailable"))

    def read_workspace_body(self):
        return self.read_json_body(MAX_WORKSPACE_BODY, "invalid_workspace_json")

    def read_json_body(self, limit, invalid):
        if len(self.headers.get_all("Content-Type", [])) != 1 or self.headers.get_content_type() != "application/json":
            raise Rejected(415, "json_required")
        lengths = self.headers.get_all("Content-Length", [])
        if (len(lengths) != 1 or not re.fullmatch(r"[1-9][0-9]{0,6}", lengths[0])
                or self.headers.get_all("Transfer-Encoding")):
            raise Rejected(413, "invalid_body_size")
        size = int(lengths[0])
        if size > limit:
            raise Rejected(413, "invalid_body_size")
        raw = self.rfile.read(size)
        if len(raw) != size:
            raise Rejected(400, invalid)
        try:
            body = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_json_object)
            encoded = workspace_json(body)
        except (ValueError, RecursionError):
            raise Rejected(400, invalid) from None
        if len(encoded) > limit or not workspace_json_safe(body):
            raise Rejected(400, invalid)
        return body

    def await_reply(self, run_id, linked=False):
        deadline = time.monotonic() + 13
        with store.listen("forge_agent_events") as listener:
            while True:
                try:
                    run = dispatch(Route.STATUS, Lookup(run_id))
                except Rejected as error:
                    if linked and error.code == "run_not_found":
                        raise Rejected(404, "chat_not_found") from None
                    raise
                if run["status"] == "completed":
                    finish_reason = run.get("finish_reason", "stop")
                    answer = store.completion_content(run["answer"], finish_reason)
                    reply = {"run_id": run_id, "model": run["model"], "choices": [
                        {"message": {"role": "assistant", "content": answer}, "finish_reason": finish_reason}
                    ]}
                    if linked:
                        reply.update(chat_id=run["chat_id"], revision=run["revision"])
                    return HTTPStatus.OK, reply
                if run["status"] in store.TERMINAL:
                    return HTTPStatus.BAD_GATEWAY, {"run_id": run_id, "error": run["status"]}
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return HTTPStatus.GATEWAY_TIMEOUT, {"run_id": run_id, "error": "reply_pending"}
                store.wait_for_signal(listener, remaining)


class Server(ThreadingHTTPServer):
    def __init__(self, token, ready, inference, *, distributed=False, pinned_model=DEFAULT_MODEL,
                 configured_models=None, tool_repositories=None, work_service=None,
                 usage_service=None, jenkins_service=None, port=PORT):
        self.authorization = f"Bearer {token}".encode()
        self.ready = ready
        self.inference = inference
        self.distributed = distributed
        self.pinned_model = pinned_model
        self.configured_models = (pinned_model,) if configured_models is None else configured_models
        self.tool_repositories = {} if tool_repositories is None else tool_repositories
        self.work_service = work_service
        self.usage_service = usage_service
        self.jenkins_service = jenkins_service
        self.slots = threading.BoundedSemaphore(24)
        super().__init__(("127.0.0.1", port), Handler)

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


def configured_models(default_model, setting):
    models = (default_model,) if setting is None else tuple(model.strip() for model in setting.split(","))
    if default_model not in models or len(set(models)) != len(models) or not set(models) <= store.MODELS:
        raise ValueError("Invalid configured runner model catalog")
    return models


def build_work_and_usage_services(environ=None, connect_factory=store.connect, *,
                                  usage_api_module=None):
    """Load/freeze independent Work and Usage configs and create selected services via factories."""
    env = os.environ if environ is None else environ
    work_cfg = work_config.load_work_config(env.get(work_config.CONFIG_ENV_VAR))
    work_service = work_api.WorkServiceFactory(work_cfg, connect_factory).create()
    usage_cfg = work_config.load_work_config(env.get(work_config.USAGE_CONFIG_ENV_VAR))
    u_api = usage_api if usage_api_module is None else usage_api_module
    store_factory = lambda cfg: usage_store.UsageStore(cfg, connect_factory)
    if u_api is None:
        if usage_cfg is not None:
            raise RuntimeError("usage_api module is required when FORGE_RUNNER_USAGE_CONFIG is configured")
        usage_service = None
    else:
        usage_service = u_api.UsageServiceFactory(usage_cfg, store_factory).create()
    return work_service, usage_service


def configured_listener(environ):
    """Validate staging controls before acquiring credentials or starting consumers."""
    raw_port = environ.get("FORGE_RUNNER_PORT", str(PORT))
    if not isinstance(raw_port, str) or not raw_port.isascii() or not raw_port.isdecimal():
        raise ValueError("Invalid configured runner port")
    port = int(raw_port)
    if not 1 <= port <= 65535:
        raise ValueError("Invalid configured runner port")
    api_only = environ.get("FORGE_RUNNER_API_ONLY", "0")
    if api_only not in {"0", "1"}:
        raise ValueError("Invalid configured API-only mode")
    return port, api_only == "1"


def build_jenkins_service(environ=None, connect_factory=store.connect, *,
                          handler_factories=None):
    """Freeze optional config before selecting and creating the ready service."""
    env = os.environ if environ is None else environ
    selected = jenkins_config.load_jenkins_config(env.get(jenkins_config.CONFIG_ENV_VAR))
    return jenkins_api.JenkinsServiceFactory(selected, connect_factory,
        handler_factories=handler_factories).create()


@contextmanager
def background_services(work_service, jenkins_service):
    """Own selected background lifetimes independently of Chat/inference mode."""
    with ExitStack() as cleanup:
        for worker in (getattr(work_service, "workflow_worker", None),
                       getattr(jenkins_service, "consumer_worker", None)):
            if worker is not None:
                # Register first: a partially acquired worker must also stop
                # if its start fails or a later service cannot become ready.
                cleanup.callback(worker.close)
                worker.start()
        yield


def main():
    port, api_only = configured_listener(os.environ)
    logging.basicConfig(level=logging.INFO)
    mode = os.environ.get("FORGE_RUNNER_MODE", "legacy")
    model = os.environ.get("FORGE_RUNNER_MODEL", DEFAULT_MODEL)
    if mode not in {"legacy", "dispatch"} or model not in store.MODELS:
        raise ValueError("Invalid configured runner mode or model")
    models = configured_models(model, os.environ.get("FORGE_RUNNER_MODELS"))
    repositories = agent_tools.repository_catalog(os.environ.get("FORGE_RUNNER_TOOL_REPOSITORIES"))
    work_service, usage_service = build_work_and_usage_services(os.environ, store.connect)
    jenkins_service = build_jenkins_service(os.environ, store.connect)
    credentials = Path(os.environ["CREDENTIALS_DIRECTORY"])
    inbound = (credentials / "runner.token").read_text().strip()
    if not re.fullmatch(r"[a-fA-F0-9]{32,128}", inbound):
        raise ValueError("Expected a hexadecimal runner bearer credential")
    inference = None
    if not api_only and (mode == "legacy" or repositories):
        outbound = (credentials / "forge.token").read_text().strip()
        if not re.fullmatch(r"[a-fA-F0-9]{32,128}", outbound):
            raise ValueError("Expected a hexadecimal Forge bearer credential")
        inference = Inference(outbound)
    ready = threading.Event()
    with Server(inbound, ready, inference, distributed=mode == "dispatch", pinned_model=model,
                configured_models=models, tool_repositories=repositories,
                work_service=work_service, usage_service=usage_service,
                jenkins_service=jenkins_service, port=port) as server:
        if api_only:
            ready.set()
        elif mode == "dispatch":
            threading.Thread(target=complete_dispatched_runs, args=(ready,), daemon=True).start()
            if repositories:
                threading.Thread(target=worker, args=(inference, threading.Event(), repositories),
                                 kwargs={"tools_only": True}, daemon=True).start()
        else:
            threading.Thread(target=worker, args=(inference, ready, repositories), daemon=True).start()
        if not api_only:
            threading.Thread(target=expire_chat_history, daemon=True).start()
        LOG.info("Listening on loopback port %d (api_only=%s)", port, api_only)
        with background_services(work_service, jenkins_service):
            server.serve_forever()


if __name__ == "__main__":
    main()
