#!/usr/bin/env bash
# forge-pg-proxy cutover (rollout step 4): make the proxy the only writer.
#
# Preparation (migration 39) is installed by an ordinary upgrade;
# cutover (migration 40) is NOT. This script is the only entry point
# for cutover, and it refuses to proceed unless every gate passes:
#   1. Migration 39 applied, migration 40 not yet applied.
#   2. Caller migration complete (rollout steps 2-3): every inventoried
#      caller is pointed at the proxy and verified in staging. The
#      script cannot prove that from here; it verifies the enforceable
#      part — the verifiers pass at the preparation stage.
#   3. Quiescence: the caller services are stopped, in-flight work has
#      drained (muse-db leases lapse within 5 minutes, GPU run leases
#      lapse within 60 seconds), and no caller session, leased
#      assignment, or leased run remains.
#   4. Cutover migration applied (revokes direct writes + muse_events
#      mutating EXECUTE), SCRAM credentials rotated (old credentials
#      stop working), existing non-proxy sessions terminated, proxy
#      HBA installed, verifiers pass at the post-cutover stage, and
#      the proxy + caller services are restarted.
#
# Usage: sudo deploy/mission-control/proxy-cutover.sh [--check]
#   --check  report whether cutover may proceed; change nothing.
# The new muse_events SCRAM password is written to a root-only file
# (never printed); the operator must give it to Muse, whose direct
# read path keeps using SCRAM. The forge_gpu_worker password is
# rotated to a fresh random value nobody needs afterwards (the GPU
# worker reaches Postgres only through the proxy after cutover).
set -euo pipefail

directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
socket_directory=/var/run/postgresql
cluster_unit=postgresql@16-main.service
credential_directory=/etc/homelab-forge/cutover-credentials

fail() {
    printf '%s\n' "$*" >&2
    exit 1
}

[[ "$(hostname -s)" == mission-control && "$EUID" == 0 ]] ||
    fail 'Run this on Mission Control with sudo.'
case "${1:-run}" in
    run|--check) action="${1:-run}" ;;
    *) fail "Usage: $0 [run|--check]" ;;
esac
[[ "$#" -le 1 ]] || fail "Usage: $0 [run|--check]"

psql_as() {
    local user="$1" database="$2"
    shift 2
    runuser -u "$user" -- env -i PATH=/usr/bin:/bin \
        psql -X -w -v ON_ERROR_STOP=1 -h "$socket_directory" -p 5432 -d "$database" "$@"
}

CALLER_ROLES="'forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','muse_proxy_writer','muse_events','forge_pg_proxy'"

migration_applied() {
    [[ "$(psql_as postgres forge_agent -Atc \
        "SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=$1 AND name='$2')")" == t ]]
}

check_quiescence() {
    # Returns 0 with a report on stdout when quiescent, 1 otherwise.
    # Caller sessions: any backend for a caller role other than this
    # script's own connection.
    local sessions leased_assignments leased_runs
    sessions="$(psql_as postgres postgres -Atc \
        "SELECT count(*) FROM pg_stat_activity WHERE usename IN ($CALLER_ROLES) AND pid <> pg_backend_pid()")"
    leased_assignments="$(psql_as postgres forge_agent -Atc \
        "SELECT count(*) FROM forge_external_agent.assignments WHERE status = 'leased'")"
    leased_runs="$(psql_as postgres forge_agent -Atc \
        "SELECT count(*) FROM forge_agent.runs WHERE lease_until IS NOT NULL AND lease_until > clock_timestamp()")"
    printf 'caller sessions: %s\nleased assignments: %s\nleased runs: %s\n' \
        "$sessions" "$leased_assignments" "$leased_runs"
    [[ "$sessions" == 0 && "$leased_assignments" == 0 && "$leased_runs" == 0 ]]
}

check_ready() {
    local ready=true
    if ! migration_applied 39 pg_proxy_preparation; then
        printf 'NOT READY: preparation migration (39) is not applied.\n'
        ready=false
    fi
    if migration_applied 40 pg_proxy_cutover; then
        printf 'NOT READY: cutover migration (40) is already applied.\n'
        ready=false
    fi
    if [[ "$ready" == true ]]; then
        # The verifiers must pass at the preparation stage first.
        psql_as postgres forge_agent -f "$directory/sql/management/verify.sql" \
            >/dev/null || fail 'NOT READY: verify.sql fails at the preparation stage.'
        printf 'verifiers: pass at the preparation stage\n'
    fi
    if ! check_quiescence; then
        printf 'NOT READY: the system is not quiescent (see counts above).\n'
        ready=false
    fi
    [[ "$ready" == true ]]
}

if [[ "$action" == --check ]]; then
    if check_ready; then
        printf 'READY: cutover may proceed.\n'
    else
        fail 'Cutover is not ready.'
    fi
    exit 0
fi

# --- cutover run ----------------------------------------------------------
migration_applied 39 pg_proxy_preparation ||
    fail 'Cutover requires the preparation migration (39) first.'
migration_applied 40 pg_proxy_cutover &&
    fail 'Cutover migration (40) is already applied; nothing to do.'

# Stop the caller services so in-flight work can drain. Remember
# which units were active so only those are restarted afterwards.
caller_units=(forge-agent-runner.service forge-coding-worker.service
    forge-gpu-worker.service forge-external-delivery.service
    forge-publication-worker.service)
stopped_units=()
for unit in "${caller_units[@]}"; do
    if systemctl is-active --quiet "$unit" 2>/dev/null; then
        systemctl stop "$unit"
        stopped_units+=("$unit")
        printf 'stopped %s\n' "$unit"
    fi
done

# Drain: muse-db leases lapse within 5 minutes; GPU run leases within
# 60 seconds. Poll quiescence for up to 6 minutes, then give up
# rather than cutting over under load.
deadline=$((SECONDS + 360))
while ! check_quiescence; do
    if [[ "$SECONDS" -ge "$deadline" ]]; then
        fail 'Quiescence did not settle within 6 minutes; callers left stopped for inspection.'
    fi
    sleep 10
done
printf 'quiescent: no caller sessions, leased assignments, or leased runs\n'

# The cutover migration revokes direct writes + muse_events mutating
# EXECUTE. The grant files re-apply the same revocation when they
# run afterwards, so a later re-provision cannot restore them.
psql_as postgres forge_agent -c 'SET ROLE forge_agent_owner' \
    -f "$directory/sql/apply-cutover.sql"
printf 'cutover migration (40) applied\n'

# Rotate the SCRAM credentials so the old ones stop working. The new
# values are written to root-only files and never printed; the
# operator must give the muse_events password to Muse (its direct
# read path keeps using SCRAM). The GPU worker needs no password
# afterwards: it reaches Postgres only through the proxy.
install -d -m 0700 "$credential_directory"
for role in forge_gpu_worker muse_events; do
    temporary="$(mktemp "$credential_directory/.password-XXXXXXXX")"
    trap 'rm -f -- "$temporary"' EXIT
    /usr/bin/python3 -c 'import secrets; print(secrets.token_urlsafe(32))' \
        > "$temporary"
    new_password="$(cat -- "$temporary")"
    psql_as postgres postgres -c \
        "ALTER ROLE $role PASSWORD '$new_password'"
    install -o root -g root -m 0600 "$temporary" \
        "$credential_directory/$role.password"
    rm -f -- "$temporary"
    trap - EXIT
    printf 'rotated SCRAM password for %s (stored in %s/%s.password)\n' \
        "$role" "$credential_directory" "$role"
done

# Terminate any remaining non-proxy sessions for the caller roles.
# The proxy is not running yet, so every such session is non-proxy
# by definition; the proxy re-establishes its own connections on
# start. The script's own backend (role postgres) is excluded.
terminated="$(psql_as postgres postgres -Atc \
    "SELECT count(*) FROM pg_stat_activity WHERE usename IN ($CALLER_ROLES) AND pid <> pg_backend_pid()")"
if [[ "$terminated" != 0 ]]; then
    psql_as postgres postgres -c \
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename IN ($CALLER_ROLES) AND pid <> pg_backend_pid()"
    printf 'terminated %s stale caller session(s)\n' "$terminated"
fi

# Reinstall the proxy HBA template, reload, and verify the
# post-cutover state (HBA rules, verify.sql, runner privileges).
"$directory/setup-postgres.sh"
printf 'provisioning verified at the post-cutover stage\n'

# Start the proxy, then restart the caller services that were
# running before. The proxy readiness check lives in
# setup-runner.sh (127.0.0.1:18087 /health).
systemctl start forge-pg-proxy.service
/usr/bin/python3 - <<'PY'
import http.client
import sys
import time

for _ in range(40):
    connection = http.client.HTTPConnection("127.0.0.1", 18087, timeout=1)
    try:
        connection.request("GET", "/health")
        response = connection.getresponse()
        response.read()
        if response.status == 200:
            print("Forge Postgres proxy ready")
            break
    except (OSError, http.client.HTTPException):
        pass
    finally:
        connection.close()
    time.sleep(0.25)
else:
    sys.exit("Forge Postgres proxy did not become ready")
PY
for unit in "${stopped_units[@]}"; do
    systemctl start "$unit"
    printf 'restarted %s\n' "$unit"
done

printf 'Cutover complete. Operator actions remaining:\n'
printf '  - Give the new muse_events SCRAM password to Muse (read it from %s/muse_events.password as root); its direct read path needs it.\n' "$credential_directory"
printf '  - Confirm each restarted caller works through the proxy (claim/renew/finish a queued assignment, one GPU inference run).\n'
