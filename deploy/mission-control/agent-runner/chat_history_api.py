"""Versioned, header-selected chat history requests for the private runner."""

from dataclasses import dataclass
from enum import Enum
from http import HTTPStatus
import re
import unicodedata
from uuid import UUID

import chat_history_store as history
from queue_store import Rejected

PATH = "/v1/dispatch"
MAX_REQUEST_BYTES = 8 * 1024
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
MAX_REVISION = 2**53 - 1
HEADERS = {"forge-envelope-version", "forge-contract-version", "forge-destination", "forge-request-id"}


class Destination(Enum):
    LIST = "ide.chat.history.list"
    OPEN = "ide.chat.history.open"
    RETENTION = "ide.chat.history.retention.set"
    DELETE = "ide.chat.history.delete"


@dataclass(frozen=True)
class ChatHistoryListRequest:
    limit: int
    cursor: str | None


@dataclass(frozen=True)
class ChatHistoryOpenRequest:
    chat_id: UUID


@dataclass(frozen=True)
class RetentionSetRequest:
    chat_id: UUID
    expected_revision: int
    policy: dict


@dataclass(frozen=True)
class DeleteChatRequest:
    chat_id: UUID
    expected_revision: int


def request_id(headers):
    values = headers.get_all("Forge-Request-Id", [])
    if len(values) != 1:
        return None
    try:
        value = values[0].encode("latin-1").decode("utf-8")
    except UnicodeError:
        return None
    if len(values[0]) > 256 or not value or any(
        character.isspace() or unicodedata.category(character) == "Cc" or character == "," for character in value
    ):
        return None
    return value


def dispatch_destination(headers):
    if (headers.get_all("Forge-Envelope-Version", []) != ["1"]
            or headers.get_all("Forge-Contract-Version", []) != ["1"]
            or any(key.lower().startswith("forge-") and key.lower() not in HEADERS for key in headers)):
        raise Rejected(400, "invalid_request")
    values = headers.get_all("Forge-Destination", [])
    if len(values) != 1:
        raise Rejected(400, "invalid_request")
    return values[0]


def destination(headers):
    value = dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def parse_chat_id(value):
    if isinstance(value, str) and re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", value):
        return UUID(value)
    raise Rejected(400, "invalid_chat_id")


def parse_revision(value):
    if type(value) is int and 1 <= value <= MAX_REVISION:
        return value
    raise Rejected(400, "invalid_revision")


def parse_retention(value):
    if value == {"kind": "long_term"}:
        return value
    if (not isinstance(value, dict)
            or value.keys() not in ({"kind", "expires_on"}, {"kind", "expires_on", "time_zone"})
            or value["kind"] != "expires"
            or not isinstance(value["expires_on"], str)
            or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", value["expires_on"])):
        raise Rejected(400, "invalid_policy")
    if "time_zone" in value and (
        not isinstance(value["time_zone"], str) or not re.fullmatch(r"[A-Za-z0-9_+./-]{1,128}", value["time_zone"])
    ):
        raise Rejected(400, "invalid_policy")
    return value


def decode(route, correlation, body):
    if not isinstance(body, dict) or body.get("request_id") != correlation:
        raise Rejected(400, "invalid_request")
    required = {"kind", "request_id"}
    if route is Destination.LIST and body.get("kind") == "chat_history_list":
        if body.keys() - required - {"limit", "cursor"}:
            raise Rejected(400, "invalid_request")
        limit, cursor = body.get("limit", 20), body.get("cursor")
        if (type(limit) is not int or not 1 <= limit <= 50
                or cursor is not None and (not isinstance(cursor, str) or not 1 <= len(cursor) <= 128)):
            raise Rejected(400, "invalid_request")
        return ChatHistoryListRequest(limit, cursor)
    required.add("chat_id")
    if route is Destination.OPEN and body.get("kind") == "chat_history_open" and body.keys() == required:
        return ChatHistoryOpenRequest(parse_chat_id(body["chat_id"]))
    required.add("expected_revision")
    if route is Destination.DELETE and body.get("kind") == "chat_history_delete" and body.keys() == required:
        return DeleteChatRequest(parse_chat_id(body["chat_id"]), parse_revision(body["expected_revision"]))
    required.add("policy")
    if route is Destination.RETENTION and body.get("kind") == "chat_history_retention_set" and body.keys() == required:
        return RetentionSetRequest(
            parse_chat_id(body["chat_id"]), parse_revision(body["expected_revision"]), parse_retention(body["policy"])
        )
    raise Rejected(400, "invalid_request")


def envelope(kind, correlation, **fields):
    return {"protocol_version": 1, "event": {"kind": kind, "request_id": correlation, **fields}}


def failed(correlation, error):
    if correlation is None:
        return error.status, {"error": "invalid_request"}
    if error.code == "invalid_policy":
        code = "invalid_policy"
    else:
        code = {
            400: "invalid_request",
            401: "denied",
            403: "denied",
            404: "not_found",
            409: "revision_conflict",
            413: "invalid_request",
            415: "invalid_request",
        }.get(error.status, "unavailable")
    return error.status, envelope("chat_history_failed", correlation, code=code)


def dispatch(route, correlation, command):
    if route is Destination.LIST and isinstance(command, ChatHistoryListRequest):
        fields = history.dao.list_chats(cursor_token=command.cursor, limit=command.limit)
        return HTTPStatus.OK, envelope("chat_history_listed", correlation, **fields)
    if route is Destination.OPEN and isinstance(command, ChatHistoryOpenRequest):
        chat = history.dao.open_chat(command.chat_id)
        return HTTPStatus.OK, envelope("chat_history_opened", correlation, chat=chat)
    if route is Destination.RETENTION and isinstance(command, RetentionSetRequest):
        fields = history.dao.set_retention(command.chat_id, command.expected_revision, command.policy)
        return HTTPStatus.OK, envelope("chat_history_retention_set", correlation, **fields)
    if route is Destination.DELETE and isinstance(command, DeleteChatRequest):
        fields = history.dao.delete_chat(command.chat_id, command.expected_revision)
        return HTTPStatus.OK, envelope("chat_history_deleted", correlation, **fields)
    raise Rejected(400, "invalid_request")
