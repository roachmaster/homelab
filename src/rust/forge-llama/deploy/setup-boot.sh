#!/usr/bin/env bash
# Run on the inference host as the account that owns the adapter.
set -euo pipefail

die() {
    printf '%s\n' "$*" >&2
    exit 1
}

usage() {
    printf 'Usage: %s [--check]\n' "${0##*/}"
    printf 'Enable the local Forge llama REST API at boot, or check its boot and API status.\n'
}

case "${1:-}" in
    -h|--help) usage; exit 0 ;;
    --check|'') ;;
    *) usage >&2; exit 2 ;;
esac
(( $# <= 1 )) || { usage >&2; exit 2; }
(( EUID != 0 )) || die "Run this as the Forge llama service user."

unit=forge-llama.service
config="$HOME/.config/forge-llama/config.json"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit_path="$unit_dir/$unit"
template="$(dirname -- "${BASH_SOURCE[0]}")/$unit"
account="$(id -un)"

[[ -x "$HOME/.local/bin/forge-llama" ]] || die "The Forge llama adapter binary is missing."
[[ -r "$config" ]] || die "The Forge llama configuration is missing or unreadable."
command -v python3 >/dev/null || die "Python 3 is required for the authenticated API check."

if [[ "${1:-}" != --check ]]; then
    [[ -r "$template" ]] || die "Keep $unit beside this script."
    mkdir -p -- "$unit_dir"
    if [[ ! -e "$unit_path" && ! -L "$unit_path" ]]; then
        temporary="$(mktemp -- "$unit_dir/.forge-llama.XXXXXX")"
        trap 'rm -f -- "$temporary"' EXIT
        install -m 0644 -- "$template" "$temporary"
        if ! ln -T -- "$temporary" "$unit_path"; then
            [[ -e "$unit_path" ]] || die "Could not install $unit."
        fi
        rm -f -- "$temporary"
        trap - EXIT
        systemctl --user daemon-reload
    fi
    cmp -s -- "$template" "$unit_path" || die "Existing $unit differs from the supplied unit. Review both files before enabling it."

    if [[ "$(loginctl show-user "$account" --property=Linger --value)" != yes ]]; then
        sudo loginctl enable-linger "$account"
    fi
    systemctl --user enable --now "$unit"
fi

[[ "$(loginctl show-user "$account" --property=Linger --value)" == yes ]] ||
    die "The user service manager is configured to stop at logout."
[[ "$(systemctl --user is-enabled "$unit")" == enabled ]] ||
    die "$unit is disabled at boot."

python3 - "$config" <<'PY'
import http.client
import json
import re
import sys
import time
from pathlib import Path

try:
    config = json.loads(Path(sys.argv[1]).read_text())
    with Path(config["token_file"]).open("rb") as token_file:
        token = token_file.read(131).rstrip(b"\n")
    if not re.fullmatch(rb"[a-fA-F0-9]{32,128}", token):
        raise ValueError
except (OSError, ValueError, KeyError, TypeError):
    sys.exit("Could not read the Forge llama credential from the local configuration.")

deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    connection = http.client.HTTPConnection("127.0.0.1", 18080, timeout=2)
    try:
        connection.request("GET", "/v1/models", headers={
            "Authorization": "Bearer " + token.decode("ascii"),
        })
        response = connection.getresponse()
        if response.status == 200:
            models = json.loads(response.read(32768))["data"]
            if isinstance(models, list):
                print(f"Forge llama API is responding on loopback ({len(models)} configured models).")
                break
        if response.status in (401, 403):
            sys.exit("The Forge llama API rejected the configured credential.")
    except (OSError, http.client.HTTPException, ValueError, KeyError, TypeError):
        pass
    finally:
        connection.close()
    time.sleep(0.25)
else:
    sys.exit("The Forge llama API did not become ready. Check journalctl --user -u forge-llama.")
PY

systemctl --user is-active --quiet "$unit" || die "$unit is inactive."
printf 'Forge llama is enabled at boot for %s.\n' "$account"
