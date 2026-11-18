"""Private Forge Studio control, isolated preview listener and build worker."""

import argparse
from dataclasses import dataclass
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import re
import signal
from subprocess import SubprocessError
from tarfile import TarError
from threading import Event
from urllib.parse import urlsplit

import psycopg2

import studio_artifact as artifact
from studio_store import Rejected, Store

PROFILE = "static-web"
ALIASES = re.compile(r"[A-Za-z0-9_.-]{1,64}")
REVISION = re.compile(r"[a-f0-9]{40}")
BUILD_ID = re.compile(r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}")
INTENT = re.compile(r"[A-Za-z0-9_.-]{1,128}")
ROUTES = {
    "game.project.list": ("game_project_list", "game_projects_listed"),
    "game.project.get": ("game_project_get", "game_project_opened"),
    "game.build.submit": ("game_build_submit", "game_build_submitted"),
    "game.build.get": ("game_build_get", "game_build_status"),
    "game.build.cancel": ("game_build_cancel", "game_build_cancelled"),
    "game.artifact.get": ("game_artifact_get", "game_artifact_opened"),
}
ACCEPTED_HEADERS = frozenset({
    "forge-envelope-version", "forge-contract-version", "forge-destination", "forge-request-id",
})


def unique(values):
    result = {}
    for key, value in values:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def matches(pattern, value):
    return isinstance(value, str) and bool(pattern.fullmatch(value))


def checked_keys(body, required, optional=()):
    if not isinstance(body, dict) or not set(required) <= body.keys() <= set(required) | set(optional):
        raise Rejected(400, "invalid_request")


@dataclass(frozen=True)
class Project:
    node: str
    project: str
    revision: str
    repository: Path
    directory: str

    def view(self):
        return {"node": self.node, "project": self.project, "revision": self.revision,
                "profiles": [PROFILE]}

    def source_recipe(self):
        encoded = json.dumps([1, PROFILE, str(self.repository), self.directory]).encode()
        return hashlib.sha256(encoded).hexdigest()


@dataclass(frozen=True)
class Catalog:
    projects: dict
    max_builds: int

    @classmethod
    def load(cls, config):
        projects = config["projects"]
        max_builds = config.get("max_builds_per_project", 256)
        if (not isinstance(projects, list) or not 1 <= len(projects) <= 32
                or type(max_builds) is not int or not 1 <= max_builds <= 4096):
            raise ValueError("invalid Studio catalog")
        approved = {}
        for value in projects:
            checked_keys(value, {"node", "project", "revision", "repository", "directory"})
            node, project, revision = (value[name] for name in ("node", "project", "revision"))
            repository, directory = Path(value["repository"]), value["directory"]
            if (not matches(ALIASES, node) or not matches(ALIASES, project)
                    or not matches(REVISION, revision) or not repository.is_absolute()
                    or not artifact.path_ok(directory) or (node, project) in approved):
                raise ValueError("invalid Studio project")
            approved[node, project] = Project(node, project, revision, repository, directory)
        return cls(approved, max_builds)

    def get(self, value):
        checked_keys(value, {"node", "project"})
        if not all(matches(ALIASES, value[key]) for key in ("node", "project")):
            raise Rejected(400, "invalid_request")
        key = value["node"], value["project"]
        if key not in self.projects:
            raise Rejected(404, "not_found")
        return key, self.projects[key]


@dataclass(frozen=True)
class Service:
    store: Store
    catalog: Catalog
    preview_origin: str
    signing_key: bytes

    def artifact(self, row):
        digest = row["artifact_digest"]
        if digest is None:
            return None
        token = artifact.sign(self.signing_key, digest)
        return {"sha256": digest, "bytes": row["artifact_bytes"],
                "preview_url": f"{self.preview_origin}/preview/{digest}/{token}/index.html"}

    def build(self, row):
        return {
            "build_id": row["build_id"],
            "project": {"node": row["node_alias"], "project": row["project_alias"]},
            "revision": row["revision"],
            "profile": row["profile"],
            "state": row["state"],
            "events": [dict(event) for event in row["events"]],
            "artifact": self.artifact(row),
        }

    def dispatch(self, destination, request_id, body):
        route = ROUTES.get(destination)
        if route is None:
            raise Rejected(404, "not_found")
        request_kind, reply_kind = route
        common = {"kind", "request_id"}
        if (not isinstance(body, dict) or body.get("kind") != request_kind
                or body.get("request_id") != request_id):
            raise Rejected(400, "invalid_request")
        status = 200
        if destination == "game.project.list":
            checked_keys(body, common)
            fields = {"projects": [project.view() for _, project in sorted(self.catalog.projects.items())]}
        else:
            key, project = self.catalog.get(body.get("project"))
            common.add("project")
            if destination == "game.project.get":
                checked_keys(body, common)
                fields = {"project": project.view()}
            elif destination == "game.build.submit":
                checked_keys(body, common | {"revision", "profile", "idempotency_key"})
                revision, profile, intent = (body[name] for name in ("revision", "profile", "idempotency_key"))
                if not matches(REVISION, revision) or profile != PROFILE or not matches(INTENT, intent):
                    raise Rejected(400, "invalid_request")
                record = self.store.submit(key, revision, profile, intent,
                                           max_builds=self.catalog.max_builds,
                                           approved_revision=project.revision,
                                           source_recipe=project.source_recipe())
                fields = {"build": self.build(record)}
                status = 202
            else:
                optional = {"after_sequence"} if destination == "game.build.get" else ()
                checked_keys(body, common | {"build_id"}, optional)
                build_id, cursor = body["build_id"], body.get("after_sequence", 0)
                if not matches(BUILD_ID, build_id) or type(cursor) is not int or not 0 <= cursor <= 2**31 - 1:
                    raise Rejected(400, "invalid_request")
                if destination == "game.build.cancel":
                    fields = {"build": self.build(self.store.cancel(key, build_id))}
                else:
                    record = self.store.status(key, build_id, cursor)
                    if destination == "game.artifact.get":
                        preview = self.artifact(record)
                        if preview is None:
                            raise Rejected(404, "not_found")
                        fields = {"artifact": preview}
                    else:
                        fields = {"build": self.build(record)}
        return status, {"kind": reply_kind, "request_id": request_id, **fields}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(5)

    def write_response(self, status, kind, payload, *, preview=False):
        try:
            self.send_response(status)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            if preview:
                self.send_header("Content-Security-Policy", artifact.CSP)
                self.send_header("Referrer-Policy", "no-referrer")
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            pass


class ApiHandler(Handler):
    def header(self, key):
        values = self.headers.get_all(key, [])
        if len(values) != 1:
            raise Rejected(400, "invalid_request")
        return values[0]

    def do_POST(self):
        request_id = None
        try:
            if self.path not in {"/v1/dispatch", "/forge/v1/dispatch"}:
                raise Rejected(404, "not_found")
            authorization = self.headers.get_all("Authorization", [])
            if len(authorization) != 1 or not hmac.compare_digest(
                    authorization[0].encode("latin-1"), self.server.authorization):
                raise Rejected(401, "unauthorized")
            value = self.header("Forge-Request-Id")
            if not re.fullmatch(r"[!-~]{1,256}", value) or "," in value:
                raise Rejected(400, "invalid_request")
            request_id = value
            if (self.header("Forge-Envelope-Version") != "1"
                    or self.header("Forge-Contract-Version") != "1"
                    or any(key.lower().startswith("forge-") and key.lower() not in ACCEPTED_HEADERS
                           for key in self.headers)):
                raise Rejected(400, "invalid_request")
            destination = self.header("Forge-Destination")
            if (self.header("Content-Type").lower() not in {
                    "application/json", "application/json; charset=utf-8"}
                    or self.headers.get_all("Transfer-Encoding", [])):
                raise Rejected(400, "invalid_request")
            length = self.header("Content-Length")
            if not re.fullmatch(r"[0-9]{1,5}", length) or not 1 <= int(length) <= 8192:
                raise Rejected(400, "invalid_request")
            body = self.rfile.read(int(length))
            if len(body) != int(length):
                raise Rejected(400, "invalid_request")
            status, reply = self.server.service.dispatch(destination, request_id, json.loads(body, object_pairs_hook=unique))
        except Rejected as error:
            status, reply = error.status, {"code": error.code}
        except (ValueError, RecursionError, TimeoutError):
            status, reply = 400, {"code": "invalid_request"}
        except psycopg2.Error:
            status, reply = 503, {"code": "unavailable"}
        if "code" in reply:
            reply = {"kind": "game_failed", "request_id": request_id, **reply}
        self.write_response(status, "application/json", json.dumps(reply).encode())


class PreviewHandler(Handler):
    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        preview = None
        parts = self.path.split("/", 4)
        if (len(parts) == 5 and parts[:2] == ["", "preview"]
                and artifact.valid_signature(self.server.signing_key, parts[2], parts[3])
                and artifact.path_ok(parts[4])):
            content = artifact.read_artifact(self.server.artifact_directory, parts[2])
            if content is not None:
                preview = artifact.asset(content, parts[4])
        if preview is None:
            self.write_response(404, "text/plain", b"Preview unavailable", preview=True)
        else:
            kind, payload = preview
            self.write_response(200, kind, payload, preview=True)


def worker(store, catalog, directory, stop):
    while not stop.is_set():
        try:
            claim = store.claim()
            if claim is None:
                stop.wait(1.0)
                continue
            project = catalog.projects.get((claim["node_alias"], claim["project_alias"]))
            if (project is None or claim["profile"] != PROFILE
                    or claim["source_recipe"] != project.source_recipe()):
                store.finish(claim)
                continue
            try:
                content = artifact.build(project.repository, claim["revision"], project.directory)
                if not store.renew(claim):
                    continue
                published = artifact.publish(directory, content)
            except (OSError, ValueError, TarError, SubprocessError):
                store.finish(claim)
            else:
                store.finish(claim, published)
        except psycopg2.Error:
            stop.wait(2.0)


def read_bounded(path, maximum=8192):
    with Path(path).open("rb") as data:
        raw = data.read(maximum + 1)
    if not 1 <= len(raw) <= maximum:
        raise ValueError("configuration size")
    return raw


def secret(path):
    raw = read_bounded(path, 256).rstrip(b"\r\n")
    if not 32 <= len(raw) <= 254 or not re.fullmatch(rb"[!-~]+", raw):
        raise ValueError("invalid Studio secret")
    return raw


def port(config, key, default):
    value = config.get(key, default)
    if type(value) is not int or not 1024 <= value <= 65535:
        raise ValueError("invalid Studio port")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("api", "worker", "preview"))
    parser.add_argument("--config", required=True)
    args = parser.parse_args()
    config = json.loads(read_bounded(args.config, 64 * 1024), object_pairs_hook=unique)
    if args.mode == "preview":
        with HTTPServer(("127.0.0.1", port(config, "preview_port", 18086)), PreviewHandler) as server:
            server.artifact_directory = Path(config["artifact_directory"]).resolve(strict=True)
            server.signing_key = secret(config["preview_key_file"])
            server.serve_forever()
        return
    catalog = Catalog.load(config)
    dsn = read_bounded(config[args.mode + "_database_file"]).decode().strip()
    store = Store(dsn)
    if args.mode == "worker":
        stop = Event()
        signal.signal(signal.SIGINT, lambda *_: stop.set())
        signal.signal(signal.SIGTERM, lambda *_: stop.set())
        worker(store, catalog, Path(config["artifact_directory"]).resolve(strict=True), stop)
        return

    preview_origin = config["preview_origin"]
    origin = urlsplit(preview_origin)
    if (origin.scheme != "https" or not origin.hostname or origin.username or origin.password
            or origin.path or origin.query or origin.fragment or not preview_origin.startswith("https://")
            or any(char.isspace() for char in preview_origin)):
        raise ValueError("configure a separate HTTPS preview origin")
    with HTTPServer(("127.0.0.1", port(config, "api_port", 18085)), ApiHandler) as server:
        token = secret(config["api_token_file"])
        if not re.fullmatch(rb"[0-9a-fA-F]{32,128}", token):
            raise ValueError("Studio API token must be hexadecimal")
        server.authorization = b"Bearer " + token
        server.service = Service(store, catalog, preview_origin, secret(config["preview_key_file"]))
        server.serve_forever()


if __name__ == "__main__":
    main()
