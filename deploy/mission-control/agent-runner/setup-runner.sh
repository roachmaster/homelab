#!/usr/bin/env bash
set -euo pipefail

[[ "$(hostname -s)" == mission-control && "$EUID" == 0 ]] ||
    { printf '%s\n' 'Run this on Mission Control with sudo.' >&2; exit 1; }
[[ "$#" -eq 0 ]] || { printf 'Usage: %s\n' "$0" >&2; exit 1; }

directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
target=/opt/homelab-forge/agent-runner
secrets_directory=/etc/homelab-forge/agent-runner
unit=/etc/systemd/system/forge-agent-runner.service

"$directory/../setup-postgres.sh" --check
test -s /etc/homelab-forge/web/llama.token
if ! /usr/bin/python3 -c 'import psycopg2'; then
    DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l \
        apt-get install -y --no-install-recommends python3-psycopg2
fi

install -d -o root -g root -m 0755 "$target"
install -d -o root -g root -m 0700 "$secrets_directory"
if [[ ! -s "$secrets_directory/token" ]]; then
    temporary="$(mktemp "$secrets_directory/.token-XXXXXXXX")"
    trap 'rm -f -- "$temporary"' EXIT
    /usr/bin/python3 -c 'import secrets; print(secrets.token_hex(32))' > "$temporary"
    mv -- "$temporary" "$secrets_directory/token"
    trap - EXIT
fi

changed=false
publish() {
    if ! cmp -s "$1" "$2"; then
        install -o root -g root -m 0644 "$1" "$2"
        changed=true
    fi
}
publish "$directory/queue_store.py" "$target/queue_store.py"
publish "$directory/runner.py" "$target/runner.py"
publish "$directory/forge-agent-runner.service" "$unit"
systemctl daemon-reload
systemctl enable forge-agent-runner.service
if [[ "$changed" == true ]] && systemctl is-active --quiet forge-agent-runner.service; then
    systemctl restart forge-agent-runner.service
fi
systemctl start forge-agent-runner.service

/usr/bin/python3 - "$secrets_directory/token" <<'PY'
from pathlib import Path
import http.client
import sys
import time

token = Path(sys.argv[1]).read_text().strip()
for _ in range(40):
    connection = http.client.HTTPConnection("127.0.0.1", 18083, timeout=1)
    try:
        connection.request("GET", "/health", headers={"Authorization": f"Bearer {token}"})
        response = connection.getresponse()
        response.read()
        if response.status == 200:
            print("Forge agent worker ready")
            break
    except (OSError, http.client.HTTPException):
        pass
    finally:
        connection.close()
    time.sleep(0.25)
else:
    sys.exit("Forge agent worker did not become ready")
PY
