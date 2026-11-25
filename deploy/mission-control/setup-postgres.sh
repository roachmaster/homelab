#!/usr/bin/env bash
set -euo pipefail

directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_directory=/mnt/data/forge-homelab/postgres
socket_directory=/var/run/postgresql
cluster_unit=postgresql@16-main.service
runner=forge_agent
loopback=127.0.0.1

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

proxy_cutover() {
    # Prints t when the cutover migration is applied, f otherwise.
    # A missing database (first install) reports f.
    psql_as postgres forge_agent -Atc \
        "SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=41 AND name='pg_proxy_cutover')" 2>/dev/null || echo f
}

proxy_preparation() {
    # Prints t when the preparation migration is applied, f otherwise.
    # A missing database (first install) reports f.
    psql_as postgres forge_agent -Atc \
        "SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=40 AND name='pg_proxy_preparation')" 2>/dev/null || echo f
}

install_auth_files() {
    # Install the HBA template and ident map for the current stage.
    # Sets hba_changed=true when a file changed. The installed HBA
    # file keeps its name; only its contents are stage-dependent.
    # Three stages: cutover (40) admits the proxy roles only via
    # peer auth and rejects everything else; preparation (39, no 40)
    # admits the proxy OS user as the caller roles via peer auth
    # while preserving direct access, so the proxy can run and be
    # verified before cutover; before preparation the original
    # template applies. The ident map is installed at preparation
    # (the peer rules need it) and at cutover.
    local template ident_file cutover preparation
    cutover="$(proxy_cutover)"
    preparation="$(proxy_preparation)"
    if [[ "$cutover" == t ]]; then
        template="$directory/postgres-worker-hba-proxy.conf"
    elif [[ "$preparation" == t ]]; then
        template="$directory/postgres-worker-hba-preparation.conf"
    else
        template="$directory/postgres-worker-hba.conf"
    fi
    if ! cmp -s "$template" "$cluster_config/forge-worker-hba.conf"; then
        install -m 0640 -o postgres -g postgres "$template" "$cluster_config/forge-worker-hba.conf"
        hba_changed=true
    fi
    if [[ "$cutover" == t || "$preparation" == t ]]; then
        ident_file="$cluster_config/pg_ident.conf"
        if [[ -f "$ident_file" ]]; then
            # Appended once; the cluster file keeps operator additions.
            if ! grep -q '^forge-pg-proxy[[:space:]]' "$ident_file"; then
                cat "$directory/postgres-worker-ident.conf" >> "$ident_file"
                hba_changed=true
            fi
        else
            install -m 0640 -o postgres -g postgres "$directory/postgres-worker-ident.conf" "$ident_file"
            hba_changed=true
        fi
    fi
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
    [[ "$configuration" == "16|$data_directory|$loopback" ]] ||
        fail "Unexpected cluster configuration: $configuration"
    local cutover preparation
    cutover="$(psql_as postgres forge_agent -Atc "SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=41 AND name='pg_proxy_cutover')")"
    preparation="$(psql_as postgres forge_agent -Atc "SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=40 AND name='pg_proxy_preparation')")"
    local worker_access
    if [[ "$cutover" == t ]]; then
        # Post-cutover: the write-capable roles are admitted only via
        # local peer auth from the proxy OS user, and muse_events keeps
        # its SCRAM host path. No host rule may name a proxy role, and
        # no local rule may admit one by another method; each peer
        # rule precedes its reject rule.
        worker_access="$(psql_as postgres postgres -Atc "
        SELECT count(*) FILTER (WHERE error IS NOT NULL) = 0
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_agent']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_coding_worker']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_coding_reconciler']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_gpu_worker']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['muse_proxy_writer']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_pg_proxy']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_agent']
             AND auth_method = 'reject') = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_coding_worker']
             AND auth_method = 'reject') = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_coding_reconciler']
             AND auth_method = 'reject') = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_gpu_worker']
             AND auth_method = 'reject') = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['muse_proxy_writer']
             AND auth_method = 'reject') = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_pg_proxy']
             AND auth_method = 'reject') = 1
           AND count(*) FILTER (WHERE type = 'host'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['muse_events']
             AND address = '$loopback' AND netmask = '255.255.255.255'
             AND auth_method = 'scram-sha-256') = 1
           AND count(*) FILTER (WHERE type = 'host' AND database = ARRAY['all']
             AND user_name = ARRAY['all'] AND auth_method = 'reject'
             AND address = '$loopback' AND netmask = '255.255.255.255') = 1
           AND count(*) FILTER (WHERE type = 'host' AND database = ARRAY['replication']
             AND user_name = ARRAY['all'] AND auth_method = 'reject'
             AND address = '$loopback' AND netmask = '255.255.255.255') = 1
           AND count(*) FILTER (WHERE type = 'host'
             AND user_name && ARRAY['forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','muse_proxy_writer','forge_pg_proxy']) = 0
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent']
             AND user_name && ARRAY['forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','muse_proxy_writer','forge_pg_proxy']
             AND auth_method NOT IN ('peer','reject')) = 0
           AND NOT EXISTS (SELECT 1 FROM pg_hba_file_rules peer
             JOIN pg_hba_file_rules rej ON peer.user_name = rej.user_name
             WHERE peer.type = 'local' AND peer.auth_method = 'peer'
             AND peer.options = ARRAY['map=forge-pg-proxy']
             AND rej.type = 'local' AND rej.auth_method = 'reject'
             AND peer.line_number >= rej.line_number)
           AND (SELECT count(*) FILTER (WHERE error IS NOT NULL) = 0
             AND count(*) FILTER (WHERE map_name = 'forge-pg-proxy'
               AND sys_name = 'forge-pg-proxy'
               AND pg_username IN ('forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','muse_proxy_writer','forge_pg_proxy')) = 6
             FROM pg_ident_file_mappings)
        FROM pg_hba_file_rules;
    ")"
    elif [[ "$preparation" == t ]]; then
        # Preparation: the proxy OS user is admitted as each caller
        # role via local peer auth (six peer rules, in order, then
        # the retained host rules at 7-10), the ident map holds the
        # six mappings, and direct access is preserved: no local
        # reject rule for a proxy role, and both SCRAM host paths
        # still admit their roles.
        worker_access="$(psql_as postgres postgres -Atc "
        SELECT count(*) FILTER (WHERE error IS NOT NULL) = 0
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_agent']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_coding_worker']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_coding_reconciler']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_gpu_worker']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['muse_proxy_writer']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_pg_proxy']
             AND auth_method = 'peer' AND options = ARRAY['map=forge-pg-proxy']) = 1
           AND count(*) FILTER (WHERE type = 'local'
             AND database = ARRAY['forge_agent']
             AND user_name && ARRAY['forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','muse_proxy_writer','forge_pg_proxy']
             AND auth_method = 'reject') = 0
           AND count(*) FILTER (WHERE rule_number = 7 AND type = 'host'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_gpu_worker']
             AND address = '$loopback' AND netmask = '255.255.255.255'
             AND auth_method = 'scram-sha-256') = 1
           AND count(*) FILTER (WHERE rule_number = 8 AND type = 'host'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['muse_events']
             AND address = '$loopback' AND netmask = '255.255.255.255'
             AND auth_method = 'scram-sha-256') = 1
           AND count(*) FILTER (WHERE rule_number = 9 AND database = ARRAY['all']
             AND user_name = ARRAY['all'] AND auth_method = 'reject'
             AND type = 'host' AND address = '$loopback' AND netmask = '255.255.255.255') = 1
           AND count(*) FILTER (WHERE rule_number = 10 AND database = ARRAY['replication']
             AND user_name = ARRAY['all'] AND auth_method = 'reject'
             AND type = 'host' AND address = '$loopback' AND netmask = '255.255.255.255') = 1
           AND (SELECT count(*) FILTER (WHERE error IS NOT NULL) = 0
             AND count(*) FILTER (WHERE map_name = 'forge-pg-proxy'
               AND sys_name = 'forge-pg-proxy'
               AND pg_username IN ('forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','muse_proxy_writer','forge_pg_proxy')) = 6
             FROM pg_ident_file_mappings)
        FROM pg_hba_file_rules;
    ")"
    else
    worker_access="$(psql_as postgres postgres -Atc "
        SELECT count(*) FILTER (WHERE error IS NOT NULL) = 0
           AND count(*) FILTER (WHERE rule_number = 1 AND type = 'host'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['forge_gpu_worker']
             AND address = '$loopback' AND netmask = '255.255.255.255'
             AND auth_method = 'scram-sha-256') = 1
           AND count(*) FILTER (WHERE rule_number = 2 AND type = 'host'
             AND database = ARRAY['forge_agent'] AND user_name = ARRAY['muse_events']
             AND address = '$loopback' AND netmask = '255.255.255.255'
             AND auth_method = 'scram-sha-256') = 1
           AND count(*) FILTER (WHERE rule_number = 3 AND database = ARRAY['all']
             AND user_name = ARRAY['all'] AND auth_method = 'reject'
             AND type = 'host' AND address = '$loopback' AND netmask = '255.255.255.255') = 1
           AND count(*) FILTER (WHERE rule_number = 4 AND database = ARRAY['replication']
             AND user_name = ARRAY['all'] AND auth_method = 'reject'
             AND type = 'host' AND address = '$loopback' AND netmask = '255.255.255.255') = 1
        FROM pg_hba_file_rules;
    ")"
    fi
    [[ "$worker_access" == t ]] || fail 'Unexpected Forge worker authentication rules.'
    psql_as postgres forge_agent -f "$directory/sql/management/verify.sql"
    # The runner connects through the proxy after cutover, so it can no
    # longer reach Postgres as its own OS user; run its checks as the
    # superuser with SET ROLE, which preserves current_user for the
    # privilege assertions at both stages.
    psql_as postgres forge_agent -c "SET ROLE forge_agent" -c "
        SELECT current_user, current_database(), current_setting('server_version') AS server_version,
               count(*) AS conversations FROM forge_agent.conversations;
    "
    psql_as postgres forge_agent -c "SET ROLE forge_agent" -f "$directory/sql/management/verify-runner-privileges.sql" \
        || fail 'Unexpected Forge database privileges.'
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
hba_changed=false
hba_file="$cluster_config/pg_hba.conf"
hba_include="include forge-worker-hba.conf"
install_auth_files
if [[ "$(head -n 1 "$hba_file")" != "$hba_include" ]]; then
    staged_hba="$(mktemp "$cluster_config/.forge-hba-XXXXXXXX")"
    trap 'unlink "$staged_hba"' EXIT
    {
        printf '%s\n' "$hba_include"
        awk -v hba_line="$hba_include" '$0 != hba_line' "$hba_file"
    } > "$staged_hba"
    chmod --reference="$hba_file" "$staged_hba"
    chown --reference="$hba_file" "$staged_hba"
    mv -T "$staged_hba" "$hba_file"
    trap - EXIT
    hba_changed=true
fi
if ! cmp -s "$directory/postgres-local.conf" "$local_config"; then
    install -m 0644 "$directory/postgres-local.conf" "$local_config"
    configuration_changed=true
fi
listen_addresses="$(runuser -u postgres -- /usr/lib/postgresql/16/bin/postgres \
    -D "$data_directory" -c "config_file=$cluster_config/postgresql.conf" -C listen_addresses)"
[[ "$listen_addresses" == "$loopback" ]] || fail "Unexpected TCP listen addresses: $listen_addresses"
systemctl daemon-reload
systemctl enable postgresql.service
if systemctl is-active --quiet "$cluster_unit"; then
    if [[ "$hba_changed" == true ]]; then
        [[ "$(psql_as postgres postgres -Atc 'SELECT count(*) FROM pg_hba_file_rules WHERE error IS NOT NULL')" == 0 ]] ||
            fail 'Invalid PostgreSQL authentication configuration.'
    fi
    if [[ "$configuration_changed" == true ]]; then
        systemctl restart "$cluster_unit"
    elif [[ "$hba_changed" == true ]]; then
        systemctl reload "$cluster_unit"
    fi
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
# apply.sql stops at the preparation migration (39); cutover (40)
# applies only via proxy-cutover.sh. Re-select the auth files after
# apply.sql so a post-cutover (or preparation-stage) re-provision
# still installs the matching HBA template.
# HBA template before verifying the cluster.
hba_changed=false
install_auth_files
if [[ "$hba_changed" == true ]]; then
    [[ "$(psql_as postgres postgres -Atc 'SELECT count(*) FROM pg_hba_file_rules WHERE error IS NOT NULL')" == 0 ]] ||
        fail 'Invalid PostgreSQL authentication configuration.'
    systemctl reload "$cluster_unit"
fi
if [[ "$(proxy_cutover)" == t ]]; then
    # Post-cutover the runner holds no direct table writes, so the
    # disposable schema fixture cannot run as the runner. Schema
    # constraints stay pinned by verify.sql.
    :
else
    psql_as postgres forge_agent -c "SET ROLE forge_agent" -f "$directory/sql/tests/schema.sql"
fi
check_cluster
