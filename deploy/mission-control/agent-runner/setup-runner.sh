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
publish "$directory/chat_history_store.py" "$target/chat_history_store.py"
publish "$directory/chat_history_api.py" "$target/chat_history_api.py"
publish "$directory/chat_run_api.py" "$target/chat_run_api.py"
publish "$directory/mail_store.py" "$target/mail_store.py"
publish "$directory/mail_api.py" "$target/mail_api.py"
publish "$directory/mail_tokenizer.py" "$target/mail_tokenizer.py"
publish "$directory/file_context.py" "$target/file_context.py"
publish "$directory/model_adapter.py" "$target/model_adapter.py"
publish "$directory/agent_tools.py" "$target/agent_tools.py"
publish "$directory/work_policy.py" "$target/work_policy.py"
publish "$directory/work_config.py" "$target/work_config.py"
publish "$directory/work_store.py" "$target/work_store.py"
publish "$directory/work_api.py" "$target/work_api.py"
publish "$directory/usage_store.py" "$target/usage_store.py"
publish "$directory/usage_api.py" "$target/usage_api.py"
publish "$directory/usage_types.py" "$target/usage_types.py"
publish "$directory/usage_rpc.py" "$target/usage_rpc.py"
publish "$directory/usage_provider.py" "$target/usage_provider.py"
publish "$directory/usage_collector.py" "$target/usage_collector.py"
publish "$directory/work_worker_store.py" "$target/work_worker_store.py"
publish "$directory/work_worker_config.py" "$target/work_worker_config.py"
publish "$directory/work_executor.py" "$target/work_executor.py"
publish "$directory/work_sessions.py" "$target/work_sessions.py"
publish "$directory/work_process.py" "$target/work_process.py"
publish "$directory/work_progress.py" "$target/work_progress.py"
publish "$directory/work_worker.py" "$target/work_worker.py"
publish "$directory/work_publication.py" "$target/work_publication.py"
publish "$directory/work_publication_config.py" "$target/work_publication_config.py"
publish "$directory/work_publication_clients.py" "$target/work_publication_clients.py"
publish "$directory/work_publication_checks.py" "$target/work_publication_checks.py"
publish "$directory/work_feedback.py" "$target/work_feedback.py"
publish "$directory/work_repair_policy.py" "$target/work_repair_policy.py"
publish "$directory/work_review.py" "$target/work_review.py"
publish "$directory/work_review_context.py" "$target/work_review_context.py"
publish "$directory/work_candidate_source.py" "$target/work_candidate_source.py"
publish "$directory/work_consumer_cache.py" "$target/work_consumer_cache.py"
publish "$directory/work_hosted_checks.py" "$target/work_hosted_checks.py"
publish "$directory/work_verification.py" "$target/work_verification.py"
publish "$directory/work_verification_init.py" "$target/work_verification_init.py"
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
