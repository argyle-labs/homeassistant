#!/usr/bin/env bash
# Registers this Home Assistant instance with orca after install.
# Run once after install.sh. Creates a long-lived access token in Home
# Assistant (Profile -> Security -> Long-lived access tokens) and registers the
# endpoint with orca so the `home-assistant.*` tools can reach it.
#
# Usage: configure.sh --orca-host <host>.local --name <endpoint-name> --token <ha-token>
set -euo pipefail

ORCA_HOST=""
NAME=""
TOKEN=""
BASE_URL=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --orca-host) ORCA_HOST="$2"; shift 2 ;;
        --name)      NAME="$2";      shift 2 ;;
        --token)     TOKEN="$2";     shift 2 ;;
        --base-url)  BASE_URL="$2";  shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$NAME"  ]] || { echo "[configure] --name is required" >&2; exit 1; }
[[ -n "$TOKEN" ]] || { echo "[configure] --token is required (HA long-lived access token)" >&2; exit 1; }

if [[ -z "$BASE_URL" ]]; then
    HOST_IP=$(hostname -I | awk '{print $1}')
    BASE_URL="http://${HOST_IP}:8123"
fi

echo "[configure] Registering endpoint '${NAME}' (${BASE_URL}) with orca..."

# Prefer the orca CLI when present; fall back to the REST tool dispatch endpoint.
if command -v orca > /dev/null 2>&1; then
    orca tool home-assistant.create \
        --name "$NAME" \
        --base-url "$BASE_URL" \
        --token "$TOKEN" \
        --enabled true
else
    : "${ORCA_HOST:?--orca-host is required when the orca CLI is not installed}"
    curl -fsSL -X POST "http://${ORCA_HOST}:12000/api/tools/home-assistant.create" \
        -H 'content-type: application/json' \
        -d "{\"name\":\"${NAME}\",\"base_url\":\"${BASE_URL}\",\"token\":\"${TOKEN}\",\"enabled\":true}"
fi

echo "[configure] Done. Try: orca tool home-assistant.entities --endpoint ${NAME}"
