#!/usr/bin/env bash
set -euo pipefail

directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_directory=/mnt/data/forge-homelab/postgres
socket_directory=/var/run/postgresql
cluster_unit=postgresql@16-main.service
runner=forge_agent

fail() {
    printf '%s\n' "$*" >&2
    exit 1
}

[[ "$(hostname -s)" == mission-control && "$EUID" == 0 ]] ||
    fail 'Run this on Mission Control with sudo.'
case "${1:-install}" in
    install|--check) action="${1:-install}" ;;
    *) fail "Usage: $0 [install|--check]" ;;
esac
[[ "$#" -le 1 ]] || fail "Usage: $0 [install|--check]"

. /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == 24.04 ]] || fail 'This deployment targets Ubuntu 24.04.'
mountpoint -q /mnt/data || fail 'Mount /mnt/data before provisioning PostgreSQL.'

exec 9>/run/lock/forge-postgres-setup.lock
flock 9

psql_as() {
    local user="$1" database="$2"
    shift 2
    runuser -u "$user" -- env -i PATH=/usr/bin:/bin \
        psql -X -w -v ON_ERROR_STOP=1 -h "$socket_directory" -p 5432 -d "$database" "$@"
}

check_cluster() {
    systemctl is-enabled --quiet postgresql.service
    systemctl is-active --quiet "$cluster_unit"
    pg_lsclusters --no-header 16 main
    pg_isready -q -h "$socket_directory" -p 5432
    local configuration
    configuration="$(psql_as postgres postgres -Atc "
        SELECT current_setting('server_version_num')::integer / 10000,
               current_setting('data_directory'), current_setting('listen_addresses');
    ")"
    [[ "$configuration" == "16|$data_directory|" ]] ||
        fail "Unexpected cluster configuration: $configuration"
    psql_as postgres forge_agent -f "$directory/sql/management/verify.sql"
    psql_as "$runner" forge_agent -c "
        SELECT current_user, current_database(), current_setting('server_version') AS server_version,
               count(*) AS conversations FROM forge_agent.conversations;
    "
    local privileges
    privileges="$(psql_as "$runner" forge_agent -Atc "
        SELECT NOT has_schema_privilege(current_user, 'forge_agent', 'CREATE')
           AND NOT has_table_privilege(current_user, 'forge_agent.schema_migrations', 'INSERT')
           AND (
               SELECT bool_and(has_table_privilege(current_user, 'forge_agent.' || relation, privilege))
               FROM unnest(ARRAY['conversations', 'runs', 'messages', 'tool_calls', 'tool_results', 'run_events']) AS relation
               CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE']) AS privilege
           );
    ")"
    [[ "$privileges" == t ]] || fail 'Unexpected Forge database privileges.'
}

if [[ "$action" == --check ]]; then
    check_cluster
    exit 0
fi

if [[ "$(dpkg-query -W -f='${db:Status-Status}' postgresql-16 2>/dev/null || true)" != installed ]]; then
    export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l
    apt-get update
    apt-get install -y --no-install-recommends postgresql-common
    install -d -m 0755 /etc/postgresql-common/createcluster.d
    cluster_override="$(mktemp /etc/postgresql-common/createcluster.d/zz-forge-XXXXXXXX.conf)"
    trap 'rm -f -- "$cluster_override"' EXIT
    printf 'create_main_cluster = false\n' > "$cluster_override"
    apt-get install -y --no-install-recommends postgresql-16
    rm -- "$cluster_override"
    trap - EXIT
fi

cluster="$(pg_lsclusters --no-header | awk '$1 == 16 && $2 == "main"')"
if [[ -z "$cluster" ]]; then
    [[ ! -e "$data_directory" && ! -L "$data_directory" ]] ||
        fail "Cluster data already exists at $data_directory. Inspect it before creating the cluster."
    install -d -m 0755 "$(dirname -- "$data_directory")"
    pg_createcluster --datadir="$data_directory" --port=5432 --start-conf=auto \
        16 main -- --auth-local=peer --auth-host=scram-sha-256
else
    read -r _ _ port _ owner location _ <<< "$cluster"
    [[ "$port" == 5432 && "$owner" == postgres && "$location" == "$data_directory" ]] ||
        fail "The existing 16/main cluster has a different configuration: $cluster"
fi

drop_in="/etc/systemd/system/$cluster_unit.d"
install -d -m 0755 "$drop_in"
install -m 0644 "$directory/postgres-storage.conf" "$drop_in/forge-storage.conf"
cluster_config=/etc/postgresql/16/main
local_config="$cluster_config/conf.d/90-forge-local.conf"
configuration_changed=false
if ! cmp -s "$directory/postgres-local.conf" "$local_config"; then
    install -m 0644 "$directory/postgres-local.conf" "$local_config"
    configuration_changed=true
fi
listen_addresses="$(runuser -u postgres -- /usr/lib/postgresql/16/bin/postgres \
    -D "$data_directory" -c "config_file=$cluster_config/postgresql.conf" -C listen_addresses)"
[[ -z "$listen_addresses" ]] || fail "Unexpected TCP listen addresses: $listen_addresses"
systemctl daemon-reload
systemctl enable postgresql.service
if [[ "$configuration_changed" == true ]] && systemctl is-active --quiet "$cluster_unit"; then
    systemctl restart "$cluster_unit"
fi
systemctl start postgresql.service "$cluster_unit"

if ! getent passwd "$runner" >/dev/null; then
    useradd --system --user-group --no-create-home --home-dir /nonexistent \
        --shell /usr/sbin/nologin "$runner"
fi
IFS=: read -r _ _ uid gid _ home shell < <(getent passwd "$runner")
[[ "$uid" != 0 && "$gid" != 0 && "$home" == /nonexistent && "$shell" == /usr/sbin/nologin &&
    "$(getent group "$runner" | cut -d: -f3)" == "$gid" && "$(id -G "$runner")" == "$gid" ]] ||
    fail "Unexpected settings for the existing $runner account."

psql_as postgres postgres -f "$directory/sql/provision.sql"
psql_as postgres forge_agent -c 'SET ROLE forge_agent_owner' -f "$directory/sql/apply.sql" \
    -f "$directory/sql/management/grant-runner.sql"
psql_as "$runner" forge_agent -f "$directory/sql/tests/schema.sql"
check_cluster
