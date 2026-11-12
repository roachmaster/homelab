#!/usr/bin/env python3
"""Compare independent Forge chat responses on fixed command fixtures."""

import argparse
from collections import Counter
from datetime import datetime, timezone
import fcntl
import http.client
import json
import os
from pathlib import Path
import pwd
import re
import socket
import subprocess
import tempfile
from time import monotonic

import yaml

from command_policy import CommandError, execute, parse_command


DRIVER = "forge-agent-driver"
MODELS = ("gpt-oss-20b", "qwen2.5-coder-7b")
TOKEN = Path("/etc/homelab-forge/web/llama.token")
RESULTS = Path("/mnt/data/forge-homelab/agent-driver/runs")
LOCK = Path("/run/forge-agent-benchmark.lock")
CASES = Path(__file__).with_name("cases.yaml")
SYSTEM = (
    "Output exactly one plain-text shell command on one line, with no explanation or Markdown. "
    "Use only wc -l, sort (optionally -u), head -n, grep (optionally -n or -F), or cut -d, -f2. "
    "You may pipe to uniq. Use only the supplied fixture filename and no path prefix."
)


class ForgeError(RuntimeError):
    pass


class Forge:
    def __init__(self):
        self.token = TOKEN.read_text().strip()
        if not self.token:
            raise ForgeError("The local Forge token is empty")

    def request(self, method, path, payload=None):
        connection = http.client.HTTPConnection("127.0.0.1", 18082, timeout=185)
        headers = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}
        body = json.dumps(payload) if payload is not None else None
        try:
            connection.request(method, path, body=body, headers=headers)
            response = connection.getresponse()
            if not 200 <= response.status < 300:
                raise ForgeError(f"{method} {path} returned HTTP {response.status}")
            data = response.read()
            return json.loads(data) if data else None
        except (OSError, http.client.HTTPException, ValueError) as error:
            raise ForgeError(f"{method} {path} failed ({type(error).__name__})") from error
        finally:
            connection.close()

    def select(self, model):
        runtime = self.request("GET", "/v1/runtime")
        if runtime.get("state") == "ready" and runtime.get("model") == model:
            return
        self.request("PUT", "/v1/runtime", {"model": model})
        runtime = self.request("GET", "/v1/runtime")
        if runtime.get("state") != "ready" or runtime.get("model") != model:
            raise ForgeError(f"Forge did not become ready with {model}")

    def restore(self, original):
        if original.get("state") == "ready":
            self.select(original["model"])
        elif self.request("GET", "/v1/runtime").get("state") != "stopped":
            self.request("DELETE", "/v1/runtime")
        restored = self.request("GET", "/v1/runtime")
        if restored.get("state") != original["state"]:
            raise ForgeError("Forge did not return to the initial state")
        return restored

    def generate(self, model, case):
        filenames = ", ".join(case["files"])
        response = self.request("POST", "/v1/chat/completions", {
            "model": model,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": f"{case['task']} Available files: {filenames}."},
            ],
            "temperature": 0.0,
            "max_tokens": 512,
            "stream": False,
        })
        try:
            choice = response["choices"][0]
            return choice["message"].get("content"), choice["finish_reason"], response.get("usage")
        except (KeyError, IndexError, AttributeError, TypeError) as error:
            raise ForgeError("Forge returned an invalid chat response") from error


def check_fixtures(cases, directory, identity):
    names = set()
    for case in cases:
        name = case["id"]
        if name in names or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}", name):
            raise ValueError(f"Invalid or duplicate case ID {name!r}")
        names.add(name)
        fixture = directory / name
        fixture.mkdir(mode=0o755)
        fixture.chmod(0o755)
        for filename, content in case["files"].items():
            if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}", filename):
                raise ValueError(f"Invalid fixture name {filename!r}")
            path = fixture / filename
            path.write_text(content, encoding="utf-8")
            path.chmod(0o444)
        reference = parse_command(case["reference"], case["files"])
        status, stdout, stderr = execute(reference, fixture, *identity)
        if status or stdout != case["stdout"]:
            raise ValueError(f"Reference failed for {name!r}: {status}, {stdout!r}, {stderr!r}")


def score(forge, model, case, directory, identity):
    row = {"model": model, "case": case["id"], "expected_stdout": case["stdout"]}
    started = monotonic()
    try:
        content, finish, usage = forge.generate(model, case)
    except ForgeError as error:
        return dict(row, result="adapter_error", error=str(error), seconds=round(monotonic() - started, 3))
    row.update(seconds=round(monotonic() - started, 3), finish_reason=finish, usage=usage)
    if not isinstance(content, str) or not content.strip() or finish != "stop":
        return dict(row, result="format_error")
    command = content.strip()
    row["command"] = command[:512]
    if len(command.splitlines()) != 1 or command.startswith("```"):
        return dict(row, result="format_error")
    try:
        commands = parse_command(command, case["files"])
        status, stdout, stderr = execute(commands, directory / case["id"], *identity)
    except CommandError as error:
        return dict(row, result="policy_rejected", error=str(error))
    except subprocess.TimeoutExpired:
        return dict(row, result="command_timeout")
    result = "passed" if stdout == case["stdout"] else "wrong_output"
    if status:
        result = "command_failed"
    return dict(row, result=result, status=status, stdout=stdout, stderr=stderr)


def run_benchmark(models, destination, cases, fixtures, identity):
    forge = Forge()
    original = forge.request("GET", "/v1/runtime")
    if original.get("state") not in ("ready", "stopped"):
        raise ForgeError("Forge must be ready or stopped before starting a comparison")
    available = {entry["id"] for entry in forge.request("GET", "/v1/models")["data"]}
    missing = set(models) - available
    if missing:
        raise ForgeError(f"Unavailable models: {', '.join(sorted(missing))}")
    destination.mkdir(mode=0o755, parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run = Path(tempfile.mkdtemp(prefix=f"{stamp}-", dir=destination))
    run.chmod(0o755)
    summary = {"initial_runtime": original, "cases_per_model": len(cases), "models": {}}
    print(f"Receipts: {run}", flush=True)
    try:
        with (run / "results.jsonl").open("w", encoding="utf-8", buffering=1) as ledger:
            for model in models:
                forge.select(model)
                counts = Counter()
                for case in cases:
                    row = score(forge, model, case, fixtures, identity)
                    ledger.write(json.dumps(row) + "\n")
                    counts[row["result"]] += 1
                    print(f"{model} {case['id']} {row['result']}", flush=True)
                summary["models"][model] = {
                    "passed": counts["passed"], "total": len(cases),
                    "accuracy": counts["passed"] / len(cases), "outcomes": dict(counts),
                }
    finally:
        try:
            summary["restored_runtime"] = forge.restore(original)
        except ForgeError as error:
            summary["restore_error"] = str(error)
            raise
        finally:
            (run / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    return int(any(item["passed"] != item["total"] for item in summary["models"].values()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Execute the fixture references only")
    parser.add_argument("--model", action="append", help="Select a model from the Forge allowlist")
    parser.add_argument("--results", type=Path, default=RESULTS, help="Directory for comparison runs")
    args = parser.parse_args()
    if socket.gethostname().split(".")[0] != "mission-control" or os.geteuid() != 0:
        parser.error("Run on Mission Control with sudo")
    try:
        account = pwd.getpwnam(DRIVER)
    except KeyError:
        parser.error("Create the driver account using setup-user.sh")
    if (
        account.pw_uid == 0 or account.pw_gid == 0 or account.pw_dir != "/nonexistent"
        or account.pw_shell != "/usr/sbin/nologin"
        or os.getgrouplist(DRIVER, account.pw_gid) != [account.pw_gid]
    ):
        parser.error("The driver account must have its own group, no home and no login shell")
    cases = yaml.safe_load(CASES.read_text(encoding="utf-8"))
    identity = (account.pw_uid, account.pw_gid)
    with tempfile.TemporaryDirectory(prefix="forge-agent-fixtures-") as temporary:
        fixtures = Path(temporary)
        fixtures.chmod(0o711)
        check_fixtures(cases, fixtures, identity)
        if args.check:
            print(f"{len(cases)} fixture references passed")
            return 0
        try:
            with LOCK.open("a") as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError as error:
                    raise ForgeError("A Forge command comparison is already running") from error
                models = dict.fromkeys(args.model or MODELS)
                return run_benchmark(models, args.results, cases, fixtures, identity)
        except ForgeError as error:
            parser.exit(2, f"{error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
