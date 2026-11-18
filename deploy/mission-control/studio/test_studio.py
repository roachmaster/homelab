"""Exercise Studio HTTP admission, real Postgres leases and immutable previews."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from dataclasses import replace
from http.client import HTTPConnection
from http.server import HTTPServer
import json
import os
from pathlib import Path
from queue import Queue
import shutil
import signal
import subprocess
from tempfile import TemporaryDirectory
from threading import Event, Thread
import time
import unittest
from urllib.parse import urlsplit

import studio
import studio_artifact as artifact
from studio_store import Store

HERE = Path(__file__).resolve().parent
TEST_TOKEN = b"0123456789abcdef" * 4
TEST_SIGNING_KEY = b"studio-owned-fixture-signing-key-for-tests"
FIXTURE_PATHS = tuple(os.environ.get(name) for name in (
    "FORGE_STUDIO_HOST_FIXTURE", "FORGE_STUDIO_ASSETS", "FORGE_STUDIO_AGENT",
))


@contextmanager
def listener(handler, **fields):
    with HTTPServer(("127.0.0.1", 0), handler) as server:
        for key, value in fields.items():
            setattr(server, key, value)
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            thread.join(timeout=2)


@contextmanager
def host_fixture(relay_port, token_path, preview_origin):
    fixture, assets, agent = FIXTURE_PATHS
    with subprocess.Popen([fixture, assets, agent, "--studio", f"127.0.0.1:{relay_port}",
                           token_path, preview_origin], stdout=subprocess.PIPE, text=True) as process:
        ready = Queue()

        def read_ready():
            ready.put((process.stdout.readline().strip(), process.stdout.readline().strip()))

        Thread(target=read_ready, daemon=True).start()
        try:
            origin, code = ready.get(timeout=10)
            if not origin.startswith("READY http://127.0.0.1:") or not code.startswith("PAIRING "):
                raise AssertionError("test host did not become ready")
            yield origin.removeprefix("READY "), code.removeprefix("PAIRING ")
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()


def host_dispatch(origin, destination, kind, fields=None, authentication=None):
    headers = {
        "Origin": origin, "Forge-Envelope-Version": "1", "Forge-Contract-Version": "1",
        "Forge-Destination": destination, "Forge-Request-Id": "fixture", "Content-Type": "application/json",
    }
    if authentication is not None:
        cookie, csrf = authentication
        headers.update({"Cookie": cookie, "Forge-CSRF": csrf})
    connection = HTTPConnection(urlsplit(origin).netloc, timeout=7)
    try:
        body = {"kind": kind, "request_id": "fixture", **(fields or {})}
        connection.request("POST", "/forge/v1/dispatch", json.dumps(body), headers)
        response = connection.getresponse()
        return response.status, response.getheader("Set-Cookie"), json.loads(response.read())
    finally:
        connection.close()


@unittest.skipUnless(shutil.which("pg_config") and shutil.which("git"), "PostgreSQL and Git fixtures")
class StudioTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bin = Path(subprocess.check_output(["pg_config", "--bindir"], text=True).strip())
        if not all((cls.bin / name).exists() for name in ("initdb", "pg_ctl", "psql")):
            raise unittest.SkipTest("local PostgreSQL binaries")
        temporary = TemporaryDirectory(prefix="forge-studio-test-")
        cls.addClassCleanup(temporary.cleanup)
        cls.root = Path(temporary.name)
        cls.repo = cls.root / "repository"
        cls.repo.mkdir()
        (cls.repo / "preview").mkdir()
        (cls.repo / "preview/index.html").write_text('<canvas></canvas><script src="play.js"></script>')
        (cls.repo / "preview/play.js").write_text("document.querySelector('canvas').width = 320;")
        subprocess.run(["git", "init", "--quiet", cls.repo], check=True, capture_output=True)
        subprocess.run(["git", "-C", cls.repo, "add", "preview"], check=True, capture_output=True)
        subprocess.run(["git", "-C", cls.repo, "-c", "user.name=Fixture",
                        "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "fixture"],
                       check=True, capture_output=True)
        cls.revision = subprocess.check_output(["git", "-C", cls.repo, "rev-parse", "HEAD"], text=True).strip()
        cls.artifacts = cls.root / "artifacts"
        cls.artifacts.mkdir()
        cls.catalog = studio.Catalog.load({"projects": [
            {"node": "lab", "project": project, "revision": cls.revision,
             "repository": str(cls.repo), "directory": "preview"}
            for project in ("game", "other")
        ]})
        cls.run_process("initdb", "-D", str(cls.root / "database"), "-A", "trust", "-U", "fixture_admin", "--no-locale")
        cls.run_process("pg_ctl", "-D", str(cls.root / "database"), "-l", str(cls.root / "database.log"),
                        "-o", f"-h '' -k {cls.root}", "-w", "start")
        cls.addClassCleanup(cls.run_process, "pg_ctl", "-D", str(cls.root / "database"), "-m", "immediate", "-w", "stop")
        cls.psql("-f", str(HERE / "schema.sql"))
        cls.psql("-f", str(HERE / "roles.sql"))
        cls.psql("-c", "CREATE ROLE fixture_api LOGIN IN ROLE forge_studio_api; "
                       "CREATE ROLE fixture_worker LOGIN IN ROLE forge_studio_worker;")
        cls.api = Store(f"host={cls.root} dbname=postgres user=fixture_api")
        cls.worker = Store(f"host={cls.root} dbname=postgres user=fixture_worker")
        cls.admin = Store(f"host={cls.root} dbname=postgres user=fixture_admin")

    @classmethod
    def run_process(cls, program, *arguments):
        subprocess.run([cls.bin / program, *arguments], check=True, capture_output=True, timeout=20)

    @classmethod
    def psql(cls, *arguments):
        cls.run_process("psql", "-h", str(cls.root), "-U", "fixture_admin", "-d", "postgres",
                        "-v", "ON_ERROR_STOP=on", *arguments)

    def setUp(self):
        with self.admin.transaction() as cursor:
            cursor.execute("TRUNCATE forge_game.build_events, forge_game.builds")

    @staticmethod
    def request(port, operation, fields=None, *, token=TEST_TOKEN, request_id="matching"):
        connection = HTTPConnection("127.0.0.1", port, timeout=5)
        body = {"kind": studio.ROUTES[operation][0], "request_id": "matching", **(fields or {})}
        headers = {
            "Authorization": "Bearer " + token.decode(),
            "Forge-Envelope-Version": "1", "Forge-Contract-Version": "1",
            "Forge-Destination": operation, "Forge-Request-Id": request_id,
            "Content-Type": "application/json",
        }
        try:
            connection.request("POST", "/v1/dispatch", body=json.dumps(body), headers=headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def enter(self, project="game", key="unique"):
        return {"project": {"node": "lab", "project": project}, "revision": self.revision,
                "profile": "static-web", "idempotency_key": key}

    def test_http_build_status_and_separate_preview(self):
        with listener(studio.PreviewHandler, artifact_directory=self.artifacts,
                      signing_key=TEST_SIGNING_KEY) as preview_port:
            service = studio.Service(self.api, self.catalog, f"http://127.0.0.1:{preview_port}", TEST_SIGNING_KEY)
            with listener(studio.ApiHandler, authorization=b"Bearer " + TEST_TOKEN, service=service) as port:
                self.assertEqual(self.request(port, "game.project.list", token=b"wrong")[0], 401)
                self.assertEqual(self.request(port, "game.project.list", request_id="mismatch")[0], 400)
                status, result = self.request(port, "game.project.list")
                self.assertEqual(status, 200)
                self.assertEqual(result["projects"][0]["profiles"], ["static-web"])
                fields = self.enter()
                status, first = self.request(port, "game.build.submit", fields)
                self.assertEqual(status, 202)
                status, second = self.request(port, "game.build.submit", fields)
                self.assertEqual((status, second), (202, first))
                self.assertEqual(self.request(port, "game.build.submit", {**fields, "revision": "0" * 40})[0], 409)
                self.assertEqual(self.request(port, "game.build.submit", self.enter(project="denied"))[0], 404)
                build_id = first["build"]["build_id"]
                query = {"project": fields["project"], "build_id": build_id}
                self.assertEqual(self.request(port, "game.build.get", {
                    **query, "project": {"node": "lab", "project": "other"}})[0], 404)
                stopping = Event()
                updated_projects = {key: replace(project, revision="a" * 40)
                                    for key, project in self.catalog.projects.items()}
                worker_catalog = replace(self.catalog, projects=updated_projects)
                worker = Thread(target=studio.worker, args=(self.worker, worker_catalog, self.artifacts, stopping))
                worker.start()
                try:
                    deadline = time.monotonic() + 5
                    while True:
                        _, status_body = self.request(port, "game.build.get", {**query, "after_sequence": 1})
                        build = status_body["build"]
                        if build["state"] == "succeeded":
                            break
                        self.assertLess(time.monotonic(), deadline)
                        stopping.wait(0.02)
                finally:
                    stopping.set()
                    worker.join(timeout=5)
                self.assertEqual(build["events"], [{"sequence": 2, "state": "running"},
                                                   {"sequence": 3, "state": "succeeded"}])
                result = self.request(port, "game.artifact.get", query)[1]["artifact"]
                self.assertEqual(result["sha256"], build["artifact"]["sha256"])
                self.assertEqual(result["bytes"], len(artifact.read_artifact(self.artifacts, result["sha256"])))
                connection = HTTPConnection("127.0.0.1", preview_port, timeout=5)
                try:
                    path = urlsplit(result["preview_url"]).path
                    connection.request("GET", path)
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.getheader("Referrer-Policy"), "no-referrer")
                    self.assertIn(b'play.js', response.read())
                    connection.request("GET", path.replace("index.html", "play.js"))
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertIn(b'320', response.read())
                    connection.request("GET", path.replace("index.html", "../index.html"))
                    response = connection.getresponse()
                    self.assertEqual(response.status, 404)
                    response.read()
                finally:
                    connection.close()

    def test_concurrent_submit_and_crash_recovery(self):
        def submit(_):
            return self.api.submit(("lab", "game"), self.revision, "static-web", "same-key",
                                   approved_revision=self.revision, max_builds=20,
                                   source_recipe=self.catalog.projects["lab", "game"].source_recipe())
        with ThreadPoolExecutor(max_workers=5) as clients:
            rows = list(clients.map(submit, range(5)))
        self.assertEqual(len({row["build_id"] for row in rows}), 1)
        first = self.worker.claim()
        with self.admin.transaction() as cursor:
            cursor.execute("UPDATE forge_game.builds SET lease_until = clock_timestamp() - INTERVAL '1 second'")
        second = self.worker.claim()
        self.assertEqual(first["build_id"], second["build_id"])
        self.assertNotEqual(first["attempt_token"], second["attempt_token"])
        self.assertFalse(self.worker.finish(first, ("0" * 64, 42)))
        self.assertTrue(self.worker.renew(second))
        cancelled = self.api.cancel(("lab", "game"), second["build_id"])
        self.assertEqual(cancelled["state"], "cancelled")
        self.assertFalse(self.worker.finish(second, ("0" * 64, 42)))
        self.assertIsNone(self.worker.claim())

    @unittest.skipUnless(all(FIXTURE_PATHS), "opt-in Rust loopback host fixture")
    def test_real_host_dispatch_through_studio_worker(self):
        catalog = studio.Catalog.load({"projects": [
            {"node": "lab", "project": project, "revision": self.revision,
             "repository": str(self.repo), "directory": "preview"}
            for project in ("workspace", "unadmitted")
        ]})
        preview_origin = "https://preview.example.invalid"
        service = studio.Service(self.api, catalog, preview_origin, TEST_SIGNING_KEY)
        token_file = self.root / "studio-fixture.token"
        token_file.touch(mode=0o600)
        token_file.write_bytes(TEST_TOKEN)
        with listener(studio.ApiHandler, authorization=b"Bearer " + TEST_TOKEN,
                      service=service) as api_port:
            with host_fixture(api_port, token_file, preview_origin) as (origin, pairing):
                status, _, _ = host_dispatch(origin, "game.project.list", "game_project_list")
                self.assertEqual(status, 401)
                status, cookie, pair = host_dispatch(origin, "ui.session.open", "pair", {"code": pairing})
                self.assertEqual(status, 200)
                auth = cookie.split(";")[0], pair["csrf"]
                status, _, catalog_response = host_dispatch(origin, "game.project.list", "game_project_list",
                                                            authentication=auth)
                self.assertEqual(status, 200)
                self.assertEqual([item["project"] for item in catalog_response["projects"]], ["workspace"])
                fields = self.enter(project="workspace")
                status, _, receipt = host_dispatch(origin, "game.build.submit", "game_build_submit", fields, auth)
                self.assertEqual(status, 202)
                self.assertEqual(receipt["build"]["state"], "queued")
                stopping = Event()
                worker = Thread(target=studio.worker, args=(self.worker, catalog, self.artifacts, stopping))
                worker.start()
                try:
                    query = {"build_id": receipt["build"]["build_id"], "project": fields["project"]}
                    deadline = time.monotonic() + 5
                    while True:
                        status, _, result = host_dispatch(origin, "game.build.get", "game_build_get", query, auth)
                        self.assertEqual(status, 200)
                        if result["build"]["state"] == "succeeded":
                            break
                        self.assertLess(time.monotonic(), deadline)
                        stopping.wait(0.02)
                    status, _, result = host_dispatch(origin, "game.artifact.get", "game_artifact_get", query, auth)
                    self.assertEqual(status, 200)
                    self.assertTrue(result["artifact"]["preview_url"].startswith(preview_origin + "/preview/"))
                    self.assertIsNotNone(artifact.read_artifact(self.artifacts, result["artifact"]["sha256"]))
                finally:
                    stopping.set()
                    worker.join(timeout=5)


if __name__ == "__main__":
    unittest.main()
