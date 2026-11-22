"""Injectable Jenkins event emitter implementations (in-process and loopback HTTP dispatch)."""

from contextlib import closing
from http.client import HTTPConnection, HTTPException
import json
import re
from typing import Protocol, runtime_checkable
from uuid import uuid4

from jenkins_events import JenkinsEventEmit, canonical_json_bytes

LOOPBACK_HOSTS = frozenset({"127.0.0.1", "::1", "localhost"})
DISPATCH_PATH = "/v1/dispatch"
EMIT_DESTINATION = "ci.jenkins.event.emit"
MAX_RESPONSE_BYTES = 64 * 1024
REQUEST_ID_RE = re.compile(r"[A-Za-z0-9_.:-]{1,128}")


class JenkinsEmitterError(Exception):
    def __init__(self, code: str, *, status: int | None = None, message: str | None = None):
        self.code = code
        self.status = status
        super().__init__(message or code)


@runtime_checkable
class JenkinsEventEmitter(Protocol):
    def emit(self, event: JenkinsEventEmit, *, request_id: str | None = None) -> dict:
        ...


class InProcessJenkinsEmitter:
    """Direct in-process emitter delegating to an injected JenkinsService."""

    def __init__(self, service):
        if service is None or not callable(getattr(service, "emit", None)):
            raise TypeError("InProcessJenkinsEmitter requires an injected service with emit()")
        self._service = service

    def emit(self, event: JenkinsEventEmit, *, request_id: str | None = None) -> dict:
        if not isinstance(event, JenkinsEventEmit):
            raise TypeError("emit requires a JenkinsEventEmit")
        if request_id is not None and (not isinstance(request_id, str) or not REQUEST_ID_RE.fullmatch(request_id)):
            raise JenkinsEmitterError("invalid_request_id")
        return self._service.emit(event)


class HttpDispatchJenkinsEmitter:
    """Authenticated loopback emitter posting typed Jenkins events to POST /v1/dispatch."""

    def __init__(
        self,
        *,
        token: str,
        host: str = "127.0.0.1",
        port: int = 18083,
        timeout_seconds: float = 10.0,
        connection_factory=HTTPConnection,
    ):
        if host not in LOOPBACK_HOSTS:
            raise ValueError("HttpDispatchJenkinsEmitter only targets loopback runner hosts")
        if type(port) is not int or not 1 <= port <= 65535:
            raise ValueError("Invalid runner port")
        if (not isinstance(token, str) or not token.strip()
                or any(ord(ch) < 0x20 or ord(ch) == 0x7F for ch in token)):
            raise ValueError("Invalid runner bearer token")
        if not isinstance(timeout_seconds, (int, float)) or not 0.5 <= float(timeout_seconds) <= 60.0:
            raise ValueError("Invalid runner timeout_seconds")
        if not callable(connection_factory):
            raise TypeError("connection_factory must be callable")
        self._host = host
        self._port = port
        self._token = token.strip()
        self._timeout = float(timeout_seconds)
        self._connection_factory = connection_factory

    def emit(self, event: JenkinsEventEmit, *, request_id: str | None = None) -> dict:
        if not isinstance(event, JenkinsEventEmit):
            raise TypeError("emit requires a JenkinsEventEmit")
        correlation = f"jk-{uuid4()}" if request_id is None else request_id
        if not isinstance(correlation, str) or not REQUEST_ID_RE.fullmatch(correlation):
            raise JenkinsEmitterError("invalid_request_id")

        payload = canonical_json_bytes(event.to_wire_dict(correlation))
        headers = {
            "Authorization": f"Bearer {self._token}",
            "Content-Type": "application/json",
            "Forge-Envelope-Version": "1",
            "Forge-Contract-Version": "1",
            "Forge-Destination": EMIT_DESTINATION,
            "Forge-Request-Id": correlation,
        }
        try:
            with closing(self._connection_factory(self._host, self._port, timeout=self._timeout)) as conn:
                conn.request("POST", DISPATCH_PATH, body=payload, headers=headers)
                response = conn.getresponse()
                status = response.status
                raw = response.read(MAX_RESPONSE_BYTES + 1)
        except (OSError, HTTPException, TimeoutError):
            raise JenkinsEmitterError("runner_unavailable") from None

        if len(raw) > MAX_RESPONSE_BYTES:
            raise JenkinsEmitterError("invalid_runner_response", status=status)
        try:
            parsed = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            raise JenkinsEmitterError("invalid_runner_response", status=status) from None

        if not isinstance(parsed, dict):
            raise JenkinsEmitterError("invalid_runner_response", status=status)

        envelope = parsed.get("event")
        if status != 202:
            code = envelope.get("code") if isinstance(envelope, dict) else parsed.get("error")
            raise JenkinsEmitterError(
                code if isinstance(code, str) else "emit_rejected",
                status=status,
            )

        if (parsed.get("protocol_version") != 1
                or not isinstance(envelope, dict)
                or envelope.get("kind") != "jenkins_event_emitted"
                or envelope.get("request_id") != correlation):
            raise JenkinsEmitterError("invalid_runner_response", status=status)

        return {
            key: value
            for key, value in envelope.items()
            if key not in {"kind", "request_id"}
        }
