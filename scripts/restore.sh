#!/usr/bin/env bash
# Restores Home Assistant /config from a backup created by backup.
# Installed as /usr/local/bin/restore by install.sh. Mirrors the orca
# `home-assistant.restore --from <tarball>` tool for shell-bootstrap use.
#
# Usage:
#   restore                        # list backups, restore latest
#   restore --list                 # list available backups and exit
#   restore <backup-file.tar.gz>   # restore specific file
#
# Options:
#   --container NAME   Docker container name (host-side invocation)
#   --force            Skip the 3-second abort window
set -euo pipefail

BACKUP_FILE=""
CONTAINER=""
FORCE=0
LIST_ONLY=0

if [[ $# -gt 0 ]] && [[ "$1" != --* ]]; then
    BACKUP_FILE="$1"
    shift
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --container)  CONTAINER="$2";  shift 2 ;;
        --force)      FORCE=1;         shift ;;
        --list)       LIST_ONLY=1;     shift ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# ── Auto-detect Home Assistant /config dir ───────────────────────────────────
if [[ -n "$CONTAINER" ]]; then
    DATA_DIR=$(docker inspect "$CONTAINER" \
        --format '{{range .Mounts}}{{if eq .Destination "/config"}}{{.Source}}{{end}}{{end}}' 2>/dev/null || true)
    [[ -n "$DATA_DIR" ]] || { echo "[restore] Error: could not determine /config volume for '${CONTAINER}'" >&2; exit 1; }
elif [[ -d /config ]]; then
    DATA_DIR="/config"
elif [[ -d /opt/homeassistant/config ]]; then
    DATA_DIR="/opt/homeassistant/config"
else
    echo "[restore] Error: Home Assistant /config dir not found. Use --container NAME for host-side Docker." >&2
    exit 1
fi

ha_stop() {
    if [[ -n "$CONTAINER" ]]; then docker stop "$CONTAINER" 2>/dev/null || true; fi
}
ha_start() {
    if [[ -n "$CONTAINER" ]]; then docker start "$CONTAINER" 2>/dev/null || true; fi
}

find_backup_dir() {
    if [[ -n "${BACKUP_DIR:-}" ]] && [[ -d "$BACKUP_DIR" ]]; then echo "$BACKUP_DIR"
    elif [[ -d /mnt/backups ]]; then echo "/mnt/backups"
    elif [[ -d /backups ]]; then echo "/backups"
    else pwd; fi
}

BACKUP_SEARCH_DIR=$(find_backup_dir)

if [[ -z "$BACKUP_FILE" ]]; then
    mapfile -t BACKUPS < <(find "$BACKUP_SEARCH_DIR" -maxdepth 1 -name 'homeassistant-config-*.tar.gz' | sort -r)
    if [[ ${#BACKUPS[@]} -eq 0 ]]; then
        echo "[restore] No backups found in ${BACKUP_SEARCH_DIR}" >&2
        exit 1
    fi
    echo "[restore] Available backups in ${BACKUP_SEARCH_DIR}:"
    for i in "${!BACKUPS[@]}"; do echo "  [$i] ${BACKUPS[$i]##*/}"; done
    echo ""
    if [[ $LIST_ONLY -eq 1 ]]; then exit 0; fi
    BACKUP_FILE="${BACKUPS[0]}"
    echo "[restore] Using latest: ${BACKUP_FILE##*/}"
fi

[[ -f "$BACKUP_FILE" ]] || { echo "[restore] Error: backup file not found: $BACKUP_FILE" >&2; exit 1; }
[[ -f "${BACKUP_FILE}.sha256" ]] || { echo "[restore] Error: checksum sidecar missing: ${BACKUP_FILE}.sha256; refusing an unverified archive" >&2; exit 1; }
# Verify and extract a private copy, so the file checked is the file extracted.
# The sidecar catches corruption, not tampering: whoever can rewrite the
# archive can rewrite the sidecar too.
EXPECTED=$(awk 'NR==1{print tolower($1)}' "${BACKUP_FILE}.sha256")
[[ "$EXPECTED" =~ ^[0-9a-f]{64}$ ]] || { echo "[restore] Error: malformed checksum sidecar" >&2; exit 1; }
PRIVATE_DIR=$(mktemp -d)
trap 'rm -rf "$PRIVATE_DIR"' EXIT
chmod 700 "$PRIVATE_DIR"
VERIFIED="${PRIVATE_DIR}/archive.tar.gz"
cp "$BACKUP_FILE" "$VERIFIED"
ACTUAL=$(sha256sum "$VERIFIED" | awk '{print $1}')
[[ "$ACTUAL" == "$EXPECTED" ]] || { echo "[restore] Error: checksum mismatch for ${BACKUP_FILE##*/}" >&2; exit 1; }
echo "[restore] Checksum verified."

if [[ $FORCE -eq 0 ]]; then
    echo "[restore] Restoring ${BACKUP_FILE##*/} in 3 seconds — Ctrl-C to abort"
    sleep 3
fi

ha_stop
trap 'ha_start; rm -rf "$PRIVATE_DIR"' EXIT

echo "[restore] Extracting to ${DATA_DIR}..."
tar -xzf "$VERIFIED" -C "$DATA_DIR"

echo "[restore] Done. Restored to: ${DATA_DIR}"
