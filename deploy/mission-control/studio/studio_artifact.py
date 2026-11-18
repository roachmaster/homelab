"""Build and serve bounded static previews from approved pinned Git revisions."""

import hashlib
import hmac
import io
import os
from pathlib import Path
import re
import selectors
import subprocess
import tarfile
import tempfile
import time
import zipfile

MAX_ARTIFACT = 16 * 1024 * 1024
MAX_ENTRIES = 256
MAX_ARCHIVE = MAX_ARTIFACT + 1024 * 1024
BUILD_SECONDS = 20
LINK_SECONDS = 900
SUFFIXES = {
    ".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8",
    ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8",
    ".wasm": "application/wasm", ".json": "application/json", ".txt": "text/plain; charset=utf-8",
    ".bin": "application/octet-stream", ".png": "image/png", ".webp": "image/webp",
    ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif",
    ".ico": "image/x-icon", ".svg": "image/svg+xml", ".ogg": "audio/ogg", ".wav": "audio/wav",
    ".woff2": "font/woff2",
}
CSP = ("default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; "
       "img-src 'self' data:; media-src 'self'; font-src 'self'; connect-src 'self'; "
       "worker-src 'self'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'; form-action 'none'")


def path_ok(path):
    return (isinstance(path, str) and 1 <= len(path) <= 240
            and all(re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,63}", part)
                    for part in path.split("/")))


def _read_archive(repository, revision, directory):
    git = "/usr/bin/git"
    deadline = time.monotonic() + BUILD_SECONDS
    with subprocess.Popen(
        [git, "--no-optional-locks", "-C", str(repository), "archive", "--format=tar",
         revision, "--", directory],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env={"PATH": "/usr/bin", "LANG": "C", "GIT_CONFIG_NOSYSTEM": "1",
             "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_TERMINAL_PROMPT": "0"},
    ) as process:
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                output = bytearray()
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not selector.select(remaining):
                        raise ValueError("git archive timed out")
                    block = os.read(process.stdout.fileno(), 65536)
                    if not block:
                        break
                    output.extend(block)
                    if len(output) > MAX_ARCHIVE:
                        raise ValueError("git archive exceeds limit")
                if process.wait(timeout=max(0, deadline - time.monotonic())):
                    raise ValueError("git archive failed")
                return output
        finally:
            if process.poll() is None:
                process.kill()


def build(repository, revision, directory):
    archive = _read_archive(repository, revision, directory)
    output = io.BytesIO()
    total = 0
    paths = set()
    prefix = directory + "/"
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as source:
        with zipfile.ZipFile(output, "w") as result:
            for member in source:
                if member.isdir():
                    continue
                relative = member.name.removeprefix(prefix)
                if (relative == member.name or not path_ok(relative) or not member.isfile()
                        or Path(relative).suffix not in SUFFIXES or relative in paths
                        or len(paths) == MAX_ENTRIES or member.size < 0):
                    raise ValueError("unsupported preview entry")
                total += member.size
                if total > MAX_ARTIFACT:
                    raise ValueError("preview files exceed limit")
                with source.extractfile(member) as content:
                    data = content.read()
                if len(data) != member.size:
                    raise ValueError("incomplete git archive")
                paths.add(relative)
                result.writestr(zipfile.ZipInfo(relative), data)
    if "index.html" not in paths or output.tell() > MAX_ARTIFACT:
        raise ValueError("preview index missing or artifact exceeds limit")
    return output.getvalue()


def publish(directory, content):
    digest = hashlib.sha256(content).hexdigest()
    destination = directory / (digest + ".zip")
    with tempfile.NamedTemporaryFile(dir=directory) as pending:
        pending.write(content)
        pending.flush()
        os.fsync(pending.fileno())
        os.fchmod(pending.fileno(), 0o640)
        try:
            os.link(pending.name, destination)
        except FileExistsError:
            if read_artifact(directory, digest) is None:
                raise ValueError("stored artifact cannot be verified") from None
        directory_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    return digest, len(content)


def read_artifact(directory, digest):
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        return None
    try:
        descriptor = os.open(directory / (digest + ".zip"), os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, "rb") as artifact:
            content = artifact.read(MAX_ARTIFACT + 1)
    except OSError:
        return None
    if len(content) > MAX_ARTIFACT or hashlib.sha256(content).hexdigest() != digest:
        return None
    return content


def sign(secret, digest, now=None):
    expires = int(time.time() if now is None else now) + LINK_SECONDS
    mac = hmac.digest(secret, f"{digest}/{expires}".encode(), "sha256").hex()
    return f"{expires}.{mac}"


def valid_signature(secret, digest, token, now=None):
    match = re.fullmatch(r"(\d{1,12})\.([0-9a-f]{64})", token)
    if not match or not re.fullmatch(r"[0-9a-f]{64}", digest):
        return False
    now = int(time.time() if now is None else now)
    expires = int(match[1])
    if not now <= expires <= now + LINK_SECONDS:
        return False
    expected = hmac.digest(secret, f"{digest}/{expires}".encode(), "sha256").hex()
    return hmac.compare_digest(expected, match[2])


def asset(content, path):
    kind = SUFFIXES.get(Path(path).suffix) if path_ok(path) else None
    if kind is None:
        return None
    try:
        with zipfile.ZipFile(io.BytesIO(content)) as archive:
            return kind, archive.read(path)
    except (KeyError, zipfile.BadZipFile):
        return None
