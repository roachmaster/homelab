#!/usr/bin/env python3
"""Create, list and revoke Homelab MCP bearer tokens (run as root on Mission Control).

The plaintext token is printed once by ``create`` and never stored; the file
keeps only its SHA-256. The running server reloads the file automatically, so
create and revoke take effect without a restart.
"""

import argparse
from datetime import timedelta
import fcntl
import grp
import json
import os
from pathlib import Path
import re
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_auth import (KNOWN_SCOPES, TOKEN_ID_PATTERN, TokenFileError, empty_store, generate_token,  # noqa: E402
                      iso, now_utc, parse_iso, read_store, token_hash)

DEFAULT_FILE = "/etc/homelab-forge/mcp/tokens.json"
DEFAULT_GROUP = "homelab_mcp"
MAX_TTL_DAYS = 365
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9 ._@:-]{0,63}")


def require_root():
    if os.geteuid() != 0 and os.environ.get("HOMELAB_MCP_TOKENS_ALLOW_NONROOT") != "1":
        sys.exit("mcp_tokens.py must run as root (sudo).")


def load(path):
    try:
        return read_store(path)[0]
    except TokenFileError as error:
        if not Path(path).exists():
            return empty_store()
        sys.exit(f"Cannot read {path}: {error}")


def save(path, data, group):
    path = Path(path)
    descriptor, temporary = tempfile.mkstemp(prefix=".tokens-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w") as target:
            os.fchmod(target.fileno(), 0o640)
            if os.geteuid() == 0:
                try:
                    gid = grp.getgrnam(group).gr_gid
                except KeyError:
                    gid = 0
                    print(f"warning: group {group} not found; the server cannot read the file until it exists",
                          file=sys.stderr)
                os.fchown(target.fileno(), 0, gid)
            json.dump(data, target, indent=2, sort_keys=True)
            target.write("\n")
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except BaseException:
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise


class Locked:
    def __init__(self, path):
        self.path = Path(str(path) + ".lock")

    def __enter__(self):
        self.handle = open(self.path, "a")
        os.chmod(self.path, 0o600)
        fcntl.flock(self.handle, fcntl.LOCK_EX)
        return self

    def __exit__(self, *exc):
        fcntl.flock(self.handle, fcntl.LOCK_UN)
        self.handle.close()


def status(record, now):
    if record.get("revoked_at"):
        return "revoked"
    return "expired" if parse_iso(record["expires_at"]) <= now else "active"


def command_create(args):
    if not NAME.fullmatch(args.name):
        sys.exit("--name must be 1-64 characters: letters, digits, space, . _ @ : -")
    if not 1 <= args.ttl_days <= MAX_TTL_DAYS:
        sys.exit(f"--ttl-days must be between 1 and {MAX_TTL_DAYS}")
    scopes = sorted(set(args.scope or ["read"]))
    if not set(scopes) <= KNOWN_SCOPES:
        sys.exit(f"unknown scope; allowed: {', '.join(sorted(KNOWN_SCOPES))}")
    Path(args.file).parent.mkdir(mode=0o750, parents=True, exist_ok=True)
    with Locked(args.file):
        data = load(args.file)
        existing = {record["id"] for record in data["tokens"]}
        token_id, token = generate_token()
        while token_id in existing:
            token_id, token = generate_token()
        created = now_utc().replace(microsecond=0)
        record = {"id": token_id, "name": args.name, "sha256": token_hash(token), "scopes": scopes,
                  "created_at": iso(created), "expires_at": iso(created + timedelta(days=args.ttl_days)),
                  "revoked_at": None}
        data["tokens"].append(record)
        save(args.file, data, args.group)
    print(f"Created token {token_id} ({args.name}), scopes {','.join(scopes)}, expires {record['expires_at']}.",
          file=sys.stderr)
    print("Copy it now; it is not stored and cannot be shown again:", file=sys.stderr)
    print(token)


def command_list(args):
    data = load(args.file)
    now = now_utc()
    rows = [{**{k: r[k] for k in ("id", "name", "scopes", "created_at", "expires_at", "revoked_at")},
             "status": status(r, now)} for r in data["tokens"]]
    if args.json:
        print(json.dumps(rows, indent=2))
        return
    if not rows:
        print("No tokens.")
        return
    print(f"{'ID':12}  {'STATUS':8}  {'EXPIRES':20}  {'SCOPES':8}  NAME")
    for row in rows:
        print(f"{row['id']:12}  {row['status']:8}  {row['expires_at']:20}  {','.join(row['scopes']):8}  {row['name']}")


def command_revoke(args):
    if not TOKEN_ID_PATTERN.fullmatch(args.id):
        sys.exit("token id must be the 12-character id shown by list")
    with Locked(args.file):
        data = load(args.file)
        for record in data["tokens"]:
            if record["id"] == args.id:
                if record.get("revoked_at"):
                    print(f"Token {args.id} was already revoked at {record['revoked_at']}.")
                    return
                record["revoked_at"] = iso(now_utc().replace(microsecond=0))
                save(args.file, data, args.group)
                print(f"Revoked token {args.id} ({record['name']}).")
                return
    sys.exit(f"No token with id {args.id}.")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--file", default=DEFAULT_FILE, help=f"token file (default {DEFAULT_FILE})")
    parser.add_argument("--group", default=DEFAULT_GROUP, help="group allowed to read the file (the service user)")
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create", help="create a token and print it once")
    create.add_argument("--name", required=True, help="who or what uses this token, e.g. 'claude-desktop laptop'")
    create.add_argument("--ttl-days", type=int, default=90, help=f"days until expiry (1-{MAX_TTL_DAYS}, default 90)")
    create.add_argument("--scope", action="append", help="scope to grant (only 'read' exists; default read)")
    listing = commands.add_parser("list", help="list tokens without secrets")
    listing.add_argument("--json", action="store_true")
    revoke = commands.add_parser("revoke", help="revoke a token by id")
    revoke.add_argument("id")
    args = parser.parse_args(argv)
    require_root()
    {"create": command_create, "list": command_list, "revoke": command_revoke}[args.command](args)


if __name__ == "__main__":
    main()
