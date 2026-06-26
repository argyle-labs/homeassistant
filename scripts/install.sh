#!/usr/bin/env bash
# Installs Home Assistant Container (Docker) on a host or LXC.
# Home Assistant is distributed as an official container image; this script
# brings up the Compose stack rather than installing from a package repo.
# Usage: install.sh [channel]   — channel: stable (default) | beta | dev
set -euo pipefail

CHANNEL="${1:-stable}"
INSTALL_DIR="${INSTALL_DIR:-/opt/homeassistant}"
COMPOSE_FILE="${INSTALL_DIR}/compose.yml"

if ! command -v docker > /dev/null 2>&1; then
    echo "[install] Installing Docker..."
    curl -fsSL https://get.docker.com | sh
fi

mkdir -p "${INSTALL_DIR}/config"

REPO_RAW="${REPO_RAW:-https://raw.githubusercontent.com/scottdkey/homeassistant/main}"
if [[ ! -f "${COMPOSE_FILE}" ]]; then
    if [[ -f "$(dirname "$0")/../compose.yml" ]]; then
        cp "$(dirname "$0")/../compose.yml" "${COMPOSE_FILE}"
    else
        curl -fsSL "${REPO_RAW}/compose.yml" -o "${COMPOSE_FILE}"
    fi
fi

# Pin the image tag to the requested channel.
sed -i "s|home-assistant:[a-z0-9.]*|home-assistant:${CHANNEL}|" "${COMPOSE_FILE}"

# Install backup and restore commands when not bundled by the Dockerfile.
if [[ "${SKIP_SCRIPT_DOWNLOAD:-0}" != "1" ]]; then
    curl -fsSL "${REPO_RAW}/scripts/backup.sh" -o /usr/local/bin/backup 2>/dev/null || true
    curl -fsSL "${REPO_RAW}/scripts/restore.sh" -o /usr/local/bin/restore 2>/dev/null || true
    chmod +x /usr/local/bin/backup /usr/local/bin/restore 2>/dev/null || true
fi

echo "[install] Starting Home Assistant (channel: ${CHANNEL})..."
docker compose -f "${COMPOSE_FILE}" up -d

HOST_IP=$(hostname -I | awk '{print $1}')
echo "[install] Done. Home Assistant is starting at http://${HOST_IP}:8123"
