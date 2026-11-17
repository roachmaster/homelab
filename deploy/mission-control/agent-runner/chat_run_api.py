"""Header-routed requests for asynchronous IDE chat runs."""

from dataclasses import dataclass
from enum import Enum
from http import HTTPStatus
import re
import unicodedata
from uuid import UUID

import chat_history_api as history_api
import queue_store as store
from queue_store import Rejected


class Destination(Enum):
    SUBMIT = "ide.chat.run.submit"
    MODELS = "ide.chat.run.models"
    EVENTS = "ide.chat.run.events"
    CANCEL = "ide.chat.run.cancel"


@dataclass(frozen=True)
class SubmitTurn:
    chat_id: UUID
    expected_revision: int | None
    content: str
    idempotency_key: str
    model_id: str | None


@dataclass(frozen=True)
class ListModels:
    pass


@dataclass(frozen=True)
class ReadEvents:
    run_id: UUID
    after_sequence: int


@dataclass(frozen=True)
class CancelRun:
    run_id: UUID


def handles(headers):
    return any(value.startswith("ide.chat.run.") for value in headers.get_all("Forge-Destination", []))


def destination(headers):
    value = history_api.dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def decode(route, correlation, body):
    if not isinstance(body, dict) or body.get("request_id") != correlation:
        raise Rejected(400, "invalid_request")
    common = {"kind", "request_id"}
    if route is Destination.MODELS and body.get("kind") == "chat_run_models" and body.keys() == common:
        return ListModels()
    if route is Destination.SUBMIT and body.get("kind") == "chat_run_submit":
        required = common | {"chat_id", "content", "idempotency_key"}
        if not required <= body.keys() <= required | {"expected_revision", "model_id"}:
            raise Rejected(400, "invalid_request")
        content = body["content"]
        key = body["idempotency_key"]
        if (not isinstance(content, str) or not content.strip() or len(content.encode()) > 2048
                or any(unicodedata.category(c) == "Cc" and c not in "\n\r\t" for c in content)
                or not isinstance(key, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", key)):
            raise Rejected(400, "invalid_request")
        revision = body.get("expected_revision")
        model_id = body.get("model_id")
        if "model_id" in body and (not isinstance(model_id, str) or model_id not in store.MODELS):
            raise Rejected(400, "invalid_request")
        return SubmitTurn(history_api.parse_chat_id(body["chat_id"]),
                          history_api.parse_revision(revision) if revision is not None else None,
                          content, key, model_id)
    common.add("run_id")
    if (route is Destination.EVENTS and body.get("kind") == "chat_run_events"
            and body.keys() == common | {"after_sequence"}):
        after = body["after_sequence"]
        if type(after) is not int or not 0 <= after <= history_api.MAX_REVISION:
            raise Rejected(400, "invalid_request")
        return ReadEvents(history_api.parse_chat_id(body["run_id"]), after)
    if route is Destination.CANCEL and body.get("kind") == "chat_run_cancel" and body.keys() == common:
        return CancelRun(history_api.parse_chat_id(body["run_id"]))
    raise Rejected(400, "invalid_request")


def failed(correlation, error):
    if correlation is None:
        return error.status, {"error": "invalid_request"}
    if error.code in {"chat_busy", "queue_full", "idempotency_conflict", "revision_conflict"}:
        code = error.code
    elif error.code in {"request_id_conflict", "conversation_busy"}:
        code = "idempotency_conflict" if error.code == "request_id_conflict" else "chat_busy"
    else:
        code = {
            400: "invalid_request", 401: "denied", 403: "denied", 404: "not_found",
            409: "revision_conflict", 413: "limit", 415: "invalid_request", 422: "limit", 429: "limit",
        }.get(error.status, "unavailable")
    return error.status, history_api.envelope("chat_run_failed", correlation, code=code)


def dispatch(route, correlation, command, model, configured_models):
    if route is Destination.MODELS and isinstance(command, ListModels):
        return HTTPStatus.OK, history_api.envelope(
            "chat_run_models_listed", correlation,
            default_model_id=model, models=store.model_catalog(configured_models),
        )
    if route is Destination.SUBMIT and isinstance(command, SubmitTurn):
        run = store.admit_chat_turn(model, command.chat_id, command.expected_revision,
                                    command.content, command.idempotency_key,
                                    model_id=command.model_id, configured_models=configured_models)
        return HTTPStatus.ACCEPTED, history_api.envelope(
            "chat_run_accepted", correlation, run_id=run["run_id"], chat_id=run["chat_id"], status=run["status"]
        )
    if route is Destination.EVENTS and isinstance(command, ReadEvents):
        page = store.get_events(command.run_id, command.after_sequence, include_completion=True)
        if not page.get("chat_id"):
            raise Rejected(404, "run_not_found")
        events = [{"sequence": row["sequence"], "kind": row["type"]} for row in page["events"]]
        reply = dict(run_id=page["run_id"], chat_id=page["chat_id"], status=page["status"],
                     next_sequence=page["next_sequence"], events=events, worker_ready=page["worker_ready"])
        if page["status"] == "completed":
            reply.update(answer=store.completion_content(page["answer"], page.get("finish_reason", "stop")),
                         revision=page["revision"])
        return HTTPStatus.OK, history_api.envelope("chat_run_events", correlation, **reply)
    if route is Destination.CANCEL and isinstance(command, CancelRun):
        run = store.cancel(command.run_id)
        return HTTPStatus.OK, history_api.envelope(
            "chat_run_cancelled", correlation, run_id=run["run_id"], chat_id=run["chat_id"], status=run["status"]
        )
    raise Rejected(400, "invalid_request")
