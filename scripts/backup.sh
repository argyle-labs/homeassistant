#!/usr/bin/env bash
# Backs up Home Assistant /config. Regenerable deps/, tts/, and the log are
# excluded. Mirrors the orca `home-assistant.backup` tool for shell-bootstrap
# use.
#
# Invocation (installed as /usr/local/bin/backup by install.sh):
#   Docker: docker exec homeassistant backup [--output DIR]
#   Host:   backup --container homeassistant --output /opt/homeassistant/backups
set -euo pipefail

OUTPUT_DIR=""
CONTAINER=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)     OUTPUT_DIR="$2";  shift 2 ;;
        --container)  CONTAINER="$2";   shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# ── Auto-detect Home Assistant /config dir ───────────────────────────────────
if [[ -n "$CONTAINER" ]]; then
    DATA_DIR=$(docker inspect "$CONTAINER" \
        --format '{{range .Mounts}}{{if eq .Destination "/config"}}{{.Source}}{{end}}{{end}}' 2>/dev/null || true)
    [[ -n "$DATA_DIR" ]] || { echo "[backup] Error: could not determine /config volume for '${CONTAINER}'" >&2; exit 1; }
elif [[ -d /config ]]; then
    DATA_DIR="/config"
elif [[ -d /opt/homeassistant/config ]]; then
    DATA_DIR="/opt/homeassistant/config"
else
    echo "[backup] Error: Home Assistant /config dir not found. Use --container NAME for host-side Docker." >&2
    exit 1
fi

if [[ -z "$OUTPUT_DIR" ]]; then
    if [[ -d /mnt/backups ]]; then OUTPUT_DIR="/mnt/backups"; else OUTPUT_DIR="$(pwd)"; fi
fi
mkdir -p "$OUTPUT_DIR"

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
OUT_FILE="${OUTPUT_DIR}/homeassistant-config-${TIMESTAMP}.tar.gz"
echo "[backup] Source: ${DATA_DIR}"
echo "[backup] Output: ${OUT_FILE}"

tar -czf "$OUT_FILE" \
    --exclude=./deps \
    --exclude=./tts \
    --exclude=./home-assistant.log \
    -C "$DATA_DIR" .

SIZE=$(du -sh "$OUT_FILE" | cut -f1)
echo "[backup] Done. ${SIZE} → ${OUT_FILE}"
