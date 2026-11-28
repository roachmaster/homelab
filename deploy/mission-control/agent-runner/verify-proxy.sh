#!/usr/bin/env bash
# Verify the configured proxy roles before the separately managed proxy cutover.
set -eu -o pipefail
[[ $EUID == 0 ]] || { echo 'Root is required for PostgreSQL peer-admission checks.' >&2; exit 1; }
url=http://127.0.0.1:18087
curl -fsS --max-time 15 --output /dev/null "$url/health"
for caller in coordinator worker reconciler gpu muse operator; do
    case $caller in
        coordinator) role=forge_agent ;;
        worker) role=forge_coding_worker ;;
        reconciler) role=forge_coding_reconciler ;;
        gpu) role=forge_gpu_worker ;;
        muse) role=muse_proxy_writer ;;
        operator) role=forge_pg_proxy ;;
    esac
    body='{"operation":"read.forge_agent.coding_tasks","args":{"limit":1}}'
    case $caller in
        gpu) body='{"operation":"fn.forge_agent_worker_api.next_model","args":{"p_models":[]}}' ;;
        muse) body='{"operation":"read.forge_external_agent.readiness","args":{"limit":1}}' ;;
    esac
    token=$(cat "/etc/homelab-forge/forge-pg-proxy/tokens/$caller.token")
    [[ -n $token && $token != *[$'\r\n']* ]]
    printf 'Authorization: Bearer %s\n' "$token" |
        curl -fsS --max-time 15 --header @- --header 'Content-Type: application/json' \
            --data-binary "$body" "$url/v1/operation" | jq -e 'has("result") and (has("error") | not)' >/dev/null
    [[ $(timeout 30 runuser -u forge-pg-proxy -- psql -X -w -h /var/run/postgresql -U "$role" -d forge_agent -Atc 'SELECT 1') == 1 ]]
    printf 'Verified %s token and PostgreSQL peer role.\n' "$caller"
done
