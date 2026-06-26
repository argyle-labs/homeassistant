#!/usr/bin/env bash
# Entrypoint wrapper for the Home Assistant image. Ensures /config exists and is
# writable, then hands off to the upstream image's launcher. The official image
# already runs HA as its own process; we only normalize the config volume.
set -euo pipefail

CONFIG_DIR="${CONFIG_DIR:-/config}"
mkdir -p "${CONFIG_DIR}"

# The upstream Home Assistant image ships its own init at /init (s6-overlay).
# Hand off to it so supervisor / add-on behavior is preserved.
if [[ -x /init ]]; then
    exec /init "$@"
fi

# Fallback: run Home Assistant directly against the config dir.
exec python3 -m homeassistant --config "${CONFIG_DIR}"
