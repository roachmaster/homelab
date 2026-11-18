"""Versioned Mail DTOs for the authenticated Forge runner."""

from dataclasses import dataclass
from enum import Enum
from http import HTTPStatus
import re
import unicodedata
from uuid import UUID

import chat_history_api as common
import mail_store
import queue_store as queue
from mail_tokenizer import TokenizerUnavailable
from queue_store import Rejected

MAX_REQUEST_BYTES = 16 * 1024


class Destination(Enum):
    LIST = "ide.mail.list"
    OPEN = "ide.mail.open"
    SEND = "ide.mail.send"
    RETRY = "ide.mail.retry"


@dataclass(frozen=True)
class ListThreads:
    archived: bool
    limit: int
    cursor: str | None
    include_metadata: bool = False


@dataclass(frozen=True)
class OpenThread:
    thread_id: UUID
    limit: int
    after_sequence: int
    include_metadata: bool = False


@dataclass(frozen=True)
class Send:
    thread_id: UUID
    expected_revision: int | None
    subject: str | None
    content: str
    idempotency_key: str


@dataclass(frozen=True)
class Retry:
    run_id: UUID
    idempotency_key: str


def handles(headers):
    return any(value.startswith("ide.mail.") for value in headers.get_all("Forge-Destination", []))


def destination(headers):
    value = common.dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def _key(value):
    if isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", value):
        return value
    raise Rejected(400, "invalid_request")


def _text(value, size, allowed_controls=""):
    if (not isinstance(value, str) or not value.strip() or len(value.encode()) > size
            or any(unicodedata.category(c) == "Cc" and c not in allowed_controls for c in value)):
        raise Rejected(400, "invalid_request")
    return value


def _limit(value, maximum):
    if type(value) is int and 1 <= value <= maximum:
        return value
    raise Rejected(400, "invalid_request")


def _include_metadata(body):
    value = body.get("include_metadata", False)
    if type(value) is not bool:
        raise Rejected(400, "invalid_request")
    return value


def decode(route, correlation, body):
    if not isinstance(body, dict) or body.get("request_id") != correlation:
        raise Rejected(400, "invalid_request")
    base = {"kind", "request_id"}
    kind = body.get("kind")
    if route is Destination.LIST and kind == "mail_list" and body.keys() <= base | {"archived", "limit", "cursor", "include_metadata"}:
        archived, cursor = body.get("archived", False), body.get("cursor")
        if (type(archived) is not bool
                or (cursor is not None and (not isinstance(cursor, str) or not 1 <= len(cursor) <= 128))):
            raise Rejected(400, "invalid_request")
        return ListThreads(archived, _limit(body.get("limit", 20), 50), cursor, _include_metadata(body))
    if route is Destination.OPEN and kind == "mail_open":
        required = base | {"thread_id"}
        if required <= body.keys() <= required | {"limit", "after_sequence", "include_metadata"}:
            after = body.get("after_sequence", 0)
            if type(after) is not int or not 0 <= after <= common.MAX_REVISION:
                raise Rejected(400, "invalid_request")
            return OpenThread(common.parse_chat_id(body["thread_id"]), _limit(body.get("limit", 10), 20),
                              after, _include_metadata(body))
    if route is Destination.SEND and kind == "mail_send":
        required = base | {"thread_id", "content", "idempotency_key"}
        if body.keys() in (required | {"subject"}, required | {"expected_revision"}):
            subject = (_text(body["subject"], mail_store.MAX_SUBJECT_BYTES) if "subject" in body else None)
            revision = common.parse_revision(body["expected_revision"]) if "expected_revision" in body else None
            return Send(common.parse_chat_id(body["thread_id"]), revision, subject,
                        _text(body["content"], mail_store.MAX_DRAFT_BYTES, "\n\r\t"),
                        _key(body["idempotency_key"]))
    if route is Destination.RETRY and kind == "mail_retry" and body.keys() == base | {"run_id", "idempotency_key"}:
        return Retry(common.parse_chat_id(body["run_id"]), _key(body["idempotency_key"]))
    raise Rejected(400, "invalid_request")


def failed(correlation, error):
    if correlation is None:
        return error.status, {"error": "invalid_request"}
    if error.code in {"context_limit", "idempotency_conflict", "revision_conflict", "thread_archived",
                      "thread_busy", "queue_full", "unavailable", "not_found"}:
        code = error.code
    elif error.status in {400, 413, 415}:
        code = "invalid_request"
    else:
        code = "unavailable"
    return error.status, common.envelope("mail_failed", correlation, code=code)


def dispatch(route, correlation, command):
    if route is Destination.LIST and isinstance(command, ListThreads):
        with queue.transaction() as cursor:
            page = mail_store.list_threads(cursor, queue.OWNER, command.limit, command.cursor, command.archived)
        if not command.include_metadata:
            page = {**page, "items": [{key: value for key, value in item.items() if key != "preview"}
                                      for item in page["items"]]}
        return HTTPStatus.OK, common.envelope("mail_listed", correlation, **page)
    if route is Destination.OPEN and isinstance(command, OpenThread):
        with queue.transaction() as cursor:
            cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            page = mail_store.open_thread(cursor, queue.OWNER, command.thread_id, command.limit, command.after_sequence)
        if page is None:
            raise Rejected(404, "not_found")
        if not command.include_metadata:
            page = {**page,
                    "thread": {key: value for key, value in page["thread"].items() if key != "preview"},
                    "messages": [{key: value for key, value in item.items() if key != "created_at"}
                                 for item in page["messages"]]}
        return HTTPStatus.OK, common.envelope("mail_opened", correlation, **page)
    try:
        if route is Destination.SEND and isinstance(command, Send):
            receipt = queue.admit_mail(command.thread_id, command.expected_revision, command.subject,
                                       command.content, command.idempotency_key)
        elif route is Destination.RETRY and isinstance(command, Retry):
            receipt = queue.retry_mail(command.run_id, command.idempotency_key)
        else:
            raise Rejected(400, "invalid_request")
    except TokenizerUnavailable:
        raise Rejected(503, "unavailable") from None
    return HTTPStatus.ACCEPTED, common.envelope("mail_sent", correlation, **receipt)
