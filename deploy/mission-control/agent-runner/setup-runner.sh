#!/usr/bin/env bash
# Provision the runner from the operator-selected immutable native release.
set -euo pipefail
umask 077
die() { printf '%s\n' "$*" >&2; exit 1; }
[[ "$(hostname -s)" == mission-control && "$EUID" == 0 ]] || die 'Run on Mission Control with sudo.'
mode=${1:-install}
[[ $# -le 1 && ( $mode == install || $mode == --check ) ]] || die "Usage: $0 [install|--check]"

directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
selected=/opt/homelab-forge/event-current
release=$(realpath -e -- "$selected") || die 'Select a qualified native event release first.'
[[ "$(dirname -- "$release")" == /opt/homelab-forge/releases ]] || die 'Unexpected native release root.'
for dir in /opt/homelab-forge /opt/homelab-forge/releases "$release" "$release/bin"; do
    [[ ! -L $dir && -d $dir && $(stat -c %u "$dir") == 0 ]] || die "Untrusted release directory $dir"
    (( (8#$(stat -c %a "$dir") & 022) == 0 )) || die "Writable release directory $dir"
done
for binary in forge-runner forge-coding-worker forge-publication-worker forge-portal-release-actor \
              forge-report-progress forge-build-info forge-attempt-checks; do
    file="$release/bin/$binary"
    [[ ! -L $file && -f $file && -x $file && $(stat -c '%u:%g:%h' "$file") == 0:0:1 ]] ||
        die "Missing or unsafe native executable $binary"
    (( (8#$(stat -c %a "$file") & 06022) == 0 )) || die "Unsafe native executable permissions $binary"
    [[ $(od -An -tx1 -N4 "$file" | tr -d ' \n') == 7f454c46 ]] || die "Expected ELF executable $binary"
done
"$directory/../setup-postgres.sh" --check

# Existing installations have per-provider configuration, revision-bound
# overrides and reservations. The coordinated rollout owns their restart.
unit=forge-agent-runner.service
if systemctl is-active --quiet "$unit"; then
    pid=$(systemctl show -p MainPID --value "$unit")
    [[ "$(readlink -e "/proc/$pid/exe")" == "$release/bin/forge-runner" ]] ||
        die 'Running event services require the coordinated drained cohort upgrade.'
fi
if [[ $mode == --check ]]; then
    printf 'Qualified native runner files verified in %s\n' "$release"
    exit 0
fi
secrets=/etc/homelab-forge/agent-runner
install -d -m 0700 -o root -g root "$secrets"
if [[ ! -e "$secrets/token" ]]; then
    temporary=$(mktemp "$secrets/.token.XXXXXXXX")
    trap 'rm -f -- "$temporary"' EXIT
    openssl rand -hex 32 > "$temporary"
    # Concurrent provisioners may only install the first token.
    ln -- "$temporary" "$secrets/token" || [[ -s "$secrets/token" ]]
    rm -f -- "$temporary"
    trap - EXIT
fi
[[ ! -L "$secrets/token" && -s "$secrets/token" ]] || die 'Invalid runner token.'
install -m 0644 -o root -g root "$directory/$unit" "/etc/systemd/system/$unit"
systemctl daemon-reload
start=$(systemctl show -p ExecStart --value "$unit")
[[ $start == "{ path=$selected/bin/forge-runner ;"* ]] || die 'A service override selects a different runner.'
systemctl enable --now "$unit"
for ((attempt=0; attempt<40; attempt++)); do
    # curl reads the authorization header from a pipe, never from argv.
    if { printf 'Authorization: Bearer '; cat "$secrets/token"; } |
        curl -fsS --max-time 1 --header @- http://127.0.0.1:18083/health 2>/dev/null |
        jq -e '.worker_ready == true' >/dev/null; then
        printf 'Forge runner is ready.\n'
        exit 0
    fi
    sleep 0.25
done
die 'Forge runner did not become ready.'
