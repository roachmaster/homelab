#!/usr/bin/python3
"""Switch the existing IDE chat relay to the runner and verify the durable run."""

import argparse
import grp
import http.client
import json
import os
from pathlib import Path
import pwd
import socket
import subprocess
import tempfile
import time
from urllib.parse import urlsplit
from uuid import uuid4

CONFIG = Path("/etc/homelab-forge/web/host-chat.json")
RUNNER_TOKEN = Path("/etc/homelab-forge/agent-runner/token")
WEB_TOKEN = CONFIG.with_name("agent-runner.token")
BACKUP = RUNNER_TOKEN.with_name("previous-ide-chat.json")
RUNNER = "127.0.0.1:18083"
DIRECT = "127.0.0.1:18082"


def install(path, data, uid, gid, mode):
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=f".{path.name}-", delete=False) as output:
        temporary = Path(output.name)
        try:
            os.fchown(output.fileno(), uid, gid)
            os.fchmod(output.fileno(), mode)
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def request(host, port, path, headers, body=None):
    connection = http.client.HTTPConnection(host, port, timeout=18)
    try:
        connection.request("POST" if body is not None else "GET", path,
                           json.dumps(body) if body is not None else None, headers)
        response = connection.getresponse()
        payload = json.loads(response.read())
        if response.status != 200:
            raise RuntimeError(f"{path} returned HTTP {response.status}")
        return payload, response.getheader("Set-Cookie")
    finally:
        connection.close()


def verify_ide(config):
    origin = config["public_origin"]
    authority = urlsplit(origin).netloc
    host, port = config["bind"].split(":")
    for _ in range(30):
        try:
            with socket.create_connection((host, int(port)), timeout=1):
                break
        except OSError:
            time.sleep(0.25)
    else:
        raise RuntimeError("IDE host listener did not start")
    base = {
        "Host": authority, "Origin": origin, "Content-Type": "application/json",
        "Forge-Envelope-Version": "1", "Forge-Contract-Version": "1",
    }
    path = "/forge/v1/dispatch"
    resume_id = f"runner-{uuid4().hex}"
    resumed, cookie = request(host, int(port), path, {
        **base, "Forge-Destination": "ui.session.resume", "Forge-Request-Id": resume_id,
    }, {"kind": "resume", "request_id": resume_id})
    if cookie is None or not resumed.get("csrf"):
        raise RuntimeError("IDE session could not be opened")
    chat_id = f"runner-{uuid4().hex}"
    prompt = f"What is 6 times 7? Reply with only the number. Reference {chat_id}."
    reply, _ = request(host, int(port), path, {
        **base, "Forge-Destination": "ide.chat", "Forge-Request-Id": chat_id,
        "Cookie": cookie.split(";", 1)[0], "Forge-CSRF": resumed["csrf"],
    }, {"kind": "chat", "request": {"request_id": chat_id,
                                    "messages": [{"role": "user", "content": prompt}]}})
    outcome = reply["event"]["outcome"]
    if outcome.get("kind") != "answer" or outcome.get("content", "").strip() != "42":
        raise RuntimeError("IDE chat did not return the expected answer")
    events = subprocess.check_output([
        "runuser", "-u", "forge_agent", "--", "psql", "-XAwqt", "-v", "ON_ERROR_STOP=1",
        "-h", "/var/run/postgresql", "-d", "forge_agent", "-v", f"prompt={prompt}",
    ], text=True, input="""
        SELECT r.id || ' ' || string_agg(e.event_type, ',' ORDER BY e.sequence)
        FROM forge_agent.runs r JOIN forge_agent.run_events e ON e.run_id = r.id
        JOIN forge_agent.messages m ON m.run_id = r.id
        WHERE m.role = 'user' AND m.content = :'prompt' AND r.status = 'completed'
        GROUP BY r.id;
    """).strip()
    if not events.endswith(" queued,model_started,completed"):
        raise RuntimeError("IDE chat did not leave the expected durable events")
    print(f"IDE chat verified: {events}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rollback", action="store_true")
    options = parser.parse_args()
    if os.geteuid() != 0 or socket.gethostname().split(".")[0] != "mission-control":
        parser.error("Run on Mission Control with sudo")
    current = CONFIG.read_bytes()
    settings = json.loads(current)
    metadata = CONFIG.stat()
    previous = settings["chat"]
    if options.rollback:
        settings["chat"] = json.loads(BACKUP.read_text())
    else:
        token = RUNNER_TOKEN.read_text().strip()
        request("127.0.0.1", 18083, "/health", {"Authorization": f"Bearer {token}"})
        if previous["endpoint"] == DIRECT:
            install(BACKUP, (json.dumps(previous) + "\n").encode(), 0, 0, 0o600)
        elif previous["endpoint"] != RUNNER:
            raise RuntimeError("IDE chat has an unexpected endpoint")
        install(WEB_TOKEN, RUNNER_TOKEN.read_bytes(), pwd.getpwnam("homelab-web").pw_uid,
                grp.getgrnam("homelab-web").gr_gid, 0o400)
        settings["chat"] = {"endpoint": RUNNER, "token_file": str(WEB_TOKEN)}
    if settings["chat"] == previous:
        if not options.rollback:
            verify_ide(settings)
        return
    args = (metadata.st_uid, metadata.st_gid, metadata.st_mode & 0o777)
    install(CONFIG, (json.dumps(settings, indent=2) + "\n").encode(), *args)
    try:
        subprocess.run(["systemctl", "restart", "homelab-web.service"], check=True)
        subprocess.run(["systemctl", "is-active", "--quiet", "homelab-web.service"], check=True)
        if not options.rollback:
            verify_ide(settings)
    except BaseException:
        install(CONFIG, current, *args)
        subprocess.run(["systemctl", "restart", "homelab-web.service"], check=True)
        raise


if __name__ == "__main__":
    main()
