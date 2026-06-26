#!/usr/bin/env bash
# Creates and configures a Home Assistant Container LXC on Proxmox VE.
# Run on the Proxmox host as root — no git clone required. Home Assistant runs
# as a Docker container inside the LXC, so the container is created with nesting
# + keyctl enabled (required for Docker-in-LXC).
#
# Usage:
#   bash <(curl -fsSL https://raw.githubusercontent.com/argyle-labs/homeassistant/main/lxc/provision.sh) <vmid> [options]
#
# Options:
#   --hostname NAME    LXC hostname (default: homeassistant)
#   --storage POOL     Proxmox storage pool for rootfs (default: local-lvm)
#   --disk SIZE        Root disk size (default: 16G)
#   --memory MB        RAM in MB (default: 2048)
#   --cores N          CPU cores (default: 2)
#   --bridge BRIDGE    Network bridge (default: vmbr0)
#   --ip IP/CIDR       Static IP with prefix (e.g. <ip>/24)
#   --gw GATEWAY       Default gateway IP
#   --config PATH      Host path for HA config (mounted at /opt/homeassistant/config)
#   --channel CHANNEL  HA image channel: stable (default) | beta | dev
#   --branch BRANCH    Repo branch to pull scripts from (default: main)
set -euo pipefail

VMID="${1:?Usage: $0 <vmid> [options]}"
shift

HOSTNAME="homeassistant"
STORAGE="local-lvm"
DISK="16G"
MEMORY="2048"
CORES="2"
BRIDGE="vmbr0"
IP=""
GW=""
CONFIG_PATH="/opt/homeassistant/config"
CHANNEL="stable"
BRANCH="main"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --hostname) HOSTNAME="$2";    shift 2 ;;
        --storage)  STORAGE="$2";     shift 2 ;;
        --disk)     DISK="$2";        shift 2 ;;
        --memory)   MEMORY="$2";      shift 2 ;;
        --cores)    CORES="$2";       shift 2 ;;
        --bridge)   BRIDGE="$2";      shift 2 ;;
        --ip)       IP="$2";          shift 2 ;;
        --gw)       GW="$2";          shift 2 ;;
        --config)   CONFIG_PATH="$2"; shift 2 ;;
        --channel)  CHANNEL="$2";     shift 2 ;;
        --branch)   BRANCH="$2";      shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

REPO_RAW="https://raw.githubusercontent.com/argyle-labs/homeassistant/${BRANCH}"
TEMPLATE_STORE="local"

TEMPLATE=$(sudo pveam list "$TEMPLATE_STORE" 2>/dev/null \
    | awk '{print $1}' | sed 's|.*vztmpl/||' \
    | grep '^debian-12-standard' | sort -V | tail -1)

if [[ -z "$TEMPLATE" ]]; then
    echo "[provision] No local Debian 12 template found, downloading..."
    sudo pveam update
    TEMPLATE=$(sudo pveam available --section system 2>/dev/null \
        | awk '{print $2}' | grep '^debian-12-standard' | sort -V | tail -1)
    [[ -n "$TEMPLATE" ]] || { echo "[provision] ERROR: No debian-12-standard template available." >&2; exit 1; }
    sudo pveam download "$TEMPLATE_STORE" "$TEMPLATE"
fi

echo "[provision] Using template: ${TEMPLATE}"

NET_ARGS="name=eth0,bridge=${BRIDGE},firewall=1"
if [[ -n "$IP" ]]; then
    NET_ARGS="${NET_ARGS},ip=${IP}"
    [[ -n "$GW" ]] && NET_ARGS="${NET_ARGS},gw=${GW}"
else
    NET_ARGS="${NET_ARGS},ip=dhcp"
fi

echo "[provision] Creating LXC ${VMID} (${HOSTNAME})..."
# Unprivileged + nesting + keyctl: the supported posture for Docker-in-LXC.
sudo pct create "$VMID" "${TEMPLATE_STORE}:vztmpl/${TEMPLATE}" \
    --hostname "$HOSTNAME" \
    --storage "$STORAGE" \
    --rootfs "${STORAGE}:${DISK}" \
    --memory "$MEMORY" \
    --cores "$CORES" \
    --net0 "$NET_ARGS" \
    --ostype debian \
    --unprivileged 1 \
    --features "nesting=1,keyctl=1" \
    --start 0

sudo mkdir -p "$CONFIG_PATH"
echo "mp0: ${CONFIG_PATH},mp=/opt/homeassistant/config" \
    | sudo tee -a "/etc/pve/lxc/${VMID}.conf" > /dev/null

echo "[provision] Starting LXC ${VMID}..."
sudo pct start "$VMID"

echo "[provision] Waiting for network..."
for _ in $(seq 1 30); do
    if sudo pct exec "$VMID" -- curl -fsSL --max-time 3 https://ghcr.io > /dev/null 2>&1; then
        break
    fi
    sleep 2
done

echo "[provision] Fetching and running install.sh (channel: ${CHANNEL})..."
sudo pct exec "$VMID" -- bash -c \
    "curl -fsSL '${REPO_RAW}/scripts/install.sh' | bash -s -- '${CHANNEL}'"

LXC_IP=$(sudo pct exec "$VMID" -- hostname -I 2>/dev/null | awk '{print $1}')
echo ""
echo "[provision] Done. LXC ${VMID} (${HOSTNAME}) is running Home Assistant."
echo "            http://${LXC_IP}:8123"
