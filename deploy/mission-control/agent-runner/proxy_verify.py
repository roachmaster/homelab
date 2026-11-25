#!/usr/bin/env python3
"""Pre-cutover proxy verification sequence (rollout steps 1-3).

Runs against the deployed proxy at the preparation stage, before
cutover, and proves the proxy actually serves every caller:

  1. /health responds 200 on the proxy port.
  2. Each caller authenticates with its token file and runs a safe
     probe through the proxy (a read for callers with reads, the
     no-claim next_model call for the GPU caller, which has no reads).
  3. The proxy OS user is admitted as each caller role via local
     peer auth (runuser as forge-pg-proxy; only when run as root).

Exit 0 when every check passes; non-zero otherwise, with one
PASS/FAIL/SKIP line per check on stdout. The HTTP probes are
importable (verify_probes) so the real-Postgres suite can run them
against the scratch-cluster proxy; the peer check is deployment-only
and is skipped (not failed) when not run as root.

Usage: proxy_verify.py [--url http://127.0.0.1:18087]
                       [--config-dir /etc/homelab-forge/forge-pg-proxy]
                       [--no-peer-check]
"""

import argparse
import http.client
import json
import os
import subprocess
import sys
import urllib.parse

DEFAULT_URL = "http://127.0.0.1:18087"
DEFAULT_CONFIG_DIR = "/etc/homelab-forge/forge-pg-proxy"
PROXY_USER = "forge-pg-proxy"

# The DB role each proxy caller connects as (from the proxy config's
# caller -> role mapping; muse connects as muse_proxy_writer).
CALLER_ROLES = {
    "coordinator": "forge_agent",
    "worker": "forge_coding_worker",
    "reconciler": "forge_coding_reconciler",
    "gpu": "forge_gpu_worker",
    "muse": "muse_proxy_writer",
    "operator": "forge_pg_proxy",
}

# A safe probe per caller: a read of one row for callers with reads,
# the no-claim next_model call for the GPU caller (it has no reads).
# Probes change no state.
PROBES = {
    "coordinator": ("read.forge_agent.coding_tasks", {"limit": 1}),
    "worker": ("read.forge_agent.coding_tasks", {"limit": 1}),
    "reconciler": ("read.forge_agent.coding_tasks", {"limit": 1}),
    "gpu": ("fn.forge_agent_worker_api.next_model", {"p_models": []}),
    "muse": ("read.forge_external_agent.readiness", {"limit": 1}),
    "operator": ("read.forge_agent.coding_tasks", {"limit": 1}),
}

CALLERS = tuple(CALLER_ROLES)


def read_token(config_dir, caller):
    """Read one caller's token file; return the bearer value or None."""
    path = os.path.join(config_dir, "tokens", f"{caller}.token")
    try:
        with open(path, encoding="utf-8") as handle:
            token = handle.read().strip()
    except OSError:
        return None
    return token or None


def _post(url, token, operation, args, timeout=15):
    parsed = urllib.parse.urlparse(url)
    conn = http.client.HTTPConnection(
        parsed.hostname, parsed.port or 80, timeout=timeout)
    try:
        body = json.dumps({"operation": operation, "args": args})
        conn.request("POST", "/v1/operation", body=body, headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        })
        response = conn.getresponse()
        payload = response.read()
    finally:
        conn.close()
    try:
        data = json.loads(payload.decode("utf-8")) if payload else {}
    except ValueError:
        data = {}
    return response.status, data


def verify_health(url, timeout=15):
    """Check /health; return (ok, detail)."""
    parsed = urllib.parse.urlparse(url)
    conn = http.client.HTTPConnection(
        parsed.hostname, parsed.port or 80, timeout=timeout)
    try:
        conn.request("GET", "/health")
        response = conn.getresponse()
        response.read()
        return response.status == 200, f"HTTP {response.status}"
    except OSError as exc:
        return False, str(exc)
    finally:
        conn.close()


def verify_probes(url, tokens, timeout=15):
    """Run the per-caller probes; return a list of (name, ok, detail).

    tokens maps caller -> bearer token string. A caller with no token
    fails its probe; a probe that returns an error payload fails.
    """
    results = []
    for caller in CALLERS:
        operation, args = PROBES[caller]
        token = tokens.get(caller)
        if not token:
            results.append((f"probe:{caller}", False, "no token file"))
            continue
        try:
            status, data = _post(url, token, operation, args, timeout)
        except OSError as exc:
            results.append((f"probe:{caller}", False, str(exc)))
            continue
        if status == 200 and "result" in data:
            results.append((f"probe:{caller}", True, operation))
        else:
            error = data.get("error", f"HTTP {status}")
            detail = error if isinstance(error, str) else f"HTTP {status}"
            results.append((f"probe:{caller}", False, detail))
    return results


def verify_peer(socket_dir="/var/run/postgresql", dbname="forge_agent"):
    """Prove the proxy OS user is admitted as each role via peer auth.

    Runs `runuser -u forge-pg-proxy -- psql -h <socket> -U <role>
    -d <dbname> -Atc 'SELECT 1'` per role; returns a list of
    (name, ok, detail). Requires root (runuser) and psql.
    """
    results = []
    for caller in CALLERS:
        role = CALLER_ROLES[caller]
        try:
            proc = subprocess.run(
                ["runuser", "-u", PROXY_USER, "--", "psql",
                 "-h", socket_dir, "-U", role, "-d", dbname,
                 "-Atc", "SELECT 1"],
                capture_output=True, text=True, timeout=30)
        except OSError as exc:
            results.append((f"peer:{caller}", False, str(exc)))
            continue
        ok = proc.returncode == 0 and proc.stdout.strip() == "1"
        detail = proc.stdout.strip() or proc.stderr.strip()
        results.append((f"peer:{caller}", ok, detail or "admitted"))
    return results


def run_checks(url, config_dir, peer_check=True, timeout=15):
    """Run the full sequence; return a list of (name, ok, detail)."""
    results = []
    ok, detail = verify_health(url, timeout)
    results.append(("health", ok, detail))
    tokens = {c: read_token(config_dir, c) for c in CALLERS}
    results.extend(verify_probes(url, tokens, timeout))
    if peer_check:
        if os.geteuid() != 0:
            results.append(("peer", None, "skipped: not root"))
        else:
            results.extend(verify_peer())
    return results


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Pre-cutover proxy verification sequence.")
    parser.add_argument("--url", default=DEFAULT_URL)
    parser.add_argument("--config-dir", default=DEFAULT_CONFIG_DIR)
    parser.add_argument("--no-peer-check", action="store_true",
                        help="skip the runuser peer-admission check")
    args = parser.parse_args(argv)

    results = run_checks(args.url, args.config_dir,
                         peer_check=not args.no_peer_check)
    failures = 0
    for name, ok, detail in results:
        state = "PASS" if ok is True else ("SKIP" if ok is None else "FAIL")
        if ok is False:
            failures += 1
        print(f"{state} {name}: {detail}")
    if failures:
        print(f"proxy-verify: {failures} check(s) failed")
        return 1
    print("proxy-verify: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
