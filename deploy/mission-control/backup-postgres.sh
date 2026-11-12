#!/usr/bin/env bash
set -euo pipefail

[[ "$(hostname -s)" == mission-control && "$EUID" == 0 ]] ||
    { printf '%s\n' 'Run this on Mission Control with sudo.' >&2; exit 1; }
[[ "$#" -eq 0 ]] || { printf 'Usage: %s\n' "$0" >&2; exit 1; }
mountpoint -q /mnt/data ||
    { printf '%s\n' 'Mount /mnt/data before backing up PostgreSQL.' >&2; exit 1; }

directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
backup_directory=/mnt/data/forge-homelab/postgres-backups
install -d -o root -g root -m 0700 "$backup_directory"
umask 077
archive="$(mktemp "$backup_directory/.forge-agent-XXXXXXXX")"
check_database="forge_agent_restore_${archive##*-}"
created=false

pg_as_admin() {
    runuser -u postgres -- env -i PATH=/usr/bin:/bin \
        "$1" -w -h /var/run/postgresql -p 5432 "${@:2}"
}

cleanup() {
    if [[ "$created" == true ]]; then
        pg_as_admin dropdb --if-exists "$check_database" ||
            printf 'Restore test database needs cleanup: %s\n' "$check_database" >&2
    fi
    rm -f -- "$archive"
}
trap cleanup EXIT

pg_as_admin pg_dump -Fc -d forge_agent > "$archive"
pg_as_admin createdb --template=template0 --owner=forge_agent_owner "$check_database"
created=true
pg_as_admin pg_restore --exit-on-error --single-transaction -d "$check_database" < "$archive"
pg_as_admin psql -X -v ON_ERROR_STOP=1 -d "$check_database" \
    -f "$directory/sql/management/verify.sql"
pg_as_admin dropdb "$check_database"
created=false
destination="$backup_directory/forge-agent-$(date -u +%Y%m%dT%H%M%SZ)-${archive##*-}.dump"
mv -- "$archive" "$destination"
printf 'Verified backup %s\n' "$destination"
