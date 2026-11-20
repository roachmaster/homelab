"""Fixed operator checks in a separate credential-free verification scope."""

import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess
import time

from work_executor import CHILD_STATE, Invocation, WorkspaceLease, unit_name
from work_process import terminate_and_reap
from work_publication_clients import PublicationError, uuid


def _exclusive_output(path):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        os.fchmod(descriptor, 0o600)
        return os.fdopen(descriptor, "wb")
    except BaseException:
        os.close(descriptor)
        raise


class IsolatedCheckRunner:
    def __init__(self, config, supervisor, *, popen=subprocess.Popen, clock=time.monotonic, timeout=1800):
        self.config, self.supervisor, self.popen = config, supervisor, popen
        self.clock, self.timeout = clock, timeout

    def quiesce(self, job):
        if not self.supervisor.stop(unit_name(uuid(job["publication_id"])))["scope_stopped"]:
            raise PublicationError("prior_verification_scope_unstopped")

    def run(self, job, repository, stop):
        # A check may create arbitrary scratch symlinks. Never reopen any of
        # them as the trusted parent on retry; each lease gets fresh scratch.
        directory = repository.parent / ("checks-" + uuid(job["lease_token"]))
        directory.mkdir(mode=0o700, exist_ok=False)
        for name in ("tmp", "cargo-home", "cargo-target", "home", "qa"):
            (directory / name).mkdir(mode=0o700, exist_ok=True)
        browser = Path("src/rust/forge-ide-web/tests/browser")
        source = self.config.worker.projects[job["project_alias"]] / browser
        target = repository / browser
        if ("/usr/bin/make", "ui-qa") in self.config.checks:
            if (not (source / "node_modules").is_dir() or not (source / "package-lock.json").is_file()
                    or not (target / "package-lock.json").is_file()
                    or hashlib.sha256((source / "package-lock.json").read_bytes()).digest()
                    != hashlib.sha256((target / "package-lock.json").read_bytes()).digest()):
                raise PublicationError("verification_dependencies_unavailable")
            support = target / "node_modules"
            if support.is_symlink() or support.exists() and any(support.iterdir()):
                raise PublicationError("verification_dependencies_changed")
            support.mkdir(exist_ok=True)
        script = directory / "verify.py"
        script.write_text("import os,subprocess\n"
                          "os.environ['GIT_CONFIG_NOSYSTEM']='1'\n"
                          "os.environ['GIT_CONFIG_GLOBAL']='/dev/null'\n"
                          "os.environ['GIT_OPTIONAL_LOCKS']='0'\n"
                          "os.environ['PYTHONDONTWRITEBYTECODE']='1'\n"
                          "os.environ['QA_OUTPUT_ROOT']='/run/forge-coding-attempt/qa'\n"
                          "commands=" + repr(self.config.checks) + "\n"
                          "for command in commands:\n"
                          " subprocess.run(command,stdin=subprocess.DEVNULL,check=True,timeout=1800)\n")
        script.chmod(0o444)
        workspace = WorkspaceLease(repository, self.config.worker.projects[job["project_alias"]],
                                   repository / ".git", job["task_id"], job["base_revision"])
        invocation = Invocation(("/usr/bin/python3", "-B", str(CHILD_STATE / "verify.py")), b"",
                                "verification", repository, directory, workspace, True)
        lease = {"attempt_id": job["publication_id"], "unit_name": unit_name(job["publication_id"])}
        process, total, complete, log = None, 0, False, None
        try:
            if stop.is_set():
                raise PublicationError("publication_lease_lost")
            # Open and protect the descriptor before any untrusted process can
            # replace the visible filename. Subsequent writes never reopen it.
            log = _exclusive_output(directory / "checks.log")
            process = self.popen(self.supervisor.argv(lease, invocation), stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, close_fds=True,
                                 start_new_session=True, env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"})
            deadline = self.clock() + self.timeout
            with log, selectors.DefaultSelector() as selected:
                for stream in (process.stdout, process.stderr):
                    os.set_blocking(stream.fileno(), False)
                    selected.register(stream, selectors.EVENT_READ)
                while selected.get_map():
                    if stop.is_set() or self.clock() >= deadline:
                        raise PublicationError("verification_timeout_or_cancelled")
                    for key, _mask in selected.select(.2):
                        try:
                            data = os.read(key.fd, 65536)
                        except BlockingIOError:
                            continue
                        if not data:
                            selected.unregister(key.fileobj)
                        else:
                            total += len(data)
                            if total > 64 * 1024 * 1024:
                                raise PublicationError("verification_output_limit")
                            log.write(data)
            if process.wait(timeout=15):
                raise PublicationError("verification_failed")
            proof = self.supervisor.stop(lease["unit_name"])
            if not terminate_and_reap(process) or not proof["scope_stopped"]:
                raise PublicationError("verification_scope_unstopped")
            # The receipt belongs outside the child-visible scratch. Exclusive
            # NOFOLLOW creation also rejects a planted path if isolation fails.
            report = repository.parent / ("checks-" + job["lease_token"] + ".json")
            with _exclusive_output(report) as receipt:
                receipt.write((json.dumps({"status": "passed", "commands": self.config.checks,
                                          "publication_id": job["publication_id"]}, indent=2) + "\n").encode())
            complete = True
        except (OSError, subprocess.SubprocessError):
            raise PublicationError("verification_process_unavailable") from None
        finally:
            try:
                if not complete:
                    self.supervisor.stop(lease["unit_name"])
            finally:
                terminate_and_reap(process)
                if log is not None:
                    log.close()
                if process is not None:
                    for stream in (process.stdout, process.stderr):
                        try:
                            stream.close()
                        except OSError:
                            pass
