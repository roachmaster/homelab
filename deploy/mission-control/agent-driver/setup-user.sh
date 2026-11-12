#!/usr/bin/env bash
set -euo pipefail

account=forge-agent-driver
if [[ "$(hostname -s)" != mission-control || "$EUID" != 0 ]]; then
    printf '%s\n' 'Run this on Mission Control with sudo.' >&2
    exit 1
fi

if ! getent passwd "$account" >/dev/null; then
    useradd --system --user-group --no-create-home \
        --home-dir /nonexistent --shell /usr/sbin/nologin "$account"
fi
IFS=: read -r _ _ uid gid _ home shell < <(getent passwd "$account")
if [[ "$uid" == 0 || "$home" != /nonexistent || "$shell" != /usr/sbin/nologin ||
      "$(getent group "$account" | cut -d: -f3)" != "$gid" ||
      "$(id -G "$account")" != "$gid" ]]; then
    printf '%s\n' 'The existing agent driver account has unexpected settings.' >&2
    exit 1
fi
printf '%s\n' "Agent driver account ready: $account"
