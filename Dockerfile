# Thin wrapper over the official Home Assistant container image. This repo does
# NOT rebuild Home Assistant — it layers the orca-managed backup/restore helpers
# and a small entrypoint that fixes /config ownership on top of the upstream
# image. Pin HASS_VERSION to `stable` / `beta` / `dev` or an explicit version.
ARG HASS_VERSION=stable
FROM ghcr.io/home-assistant/home-assistant:${HASS_VERSION}

COPY scripts/entrypoint.sh /entrypoint.sh
COPY scripts/backup.sh /usr/local/bin/backup
COPY scripts/restore.sh /usr/local/bin/restore
RUN chmod +x /entrypoint.sh /usr/local/bin/backup /usr/local/bin/restore

EXPOSE 8123

VOLUME ["/config"]

HEALTHCHECK --interval=30s --timeout=10s --start-period=120s --retries=3 \
    CMD curl -fsSL http://localhost:8123/manifest.json > /dev/null || exit 1

ENTRYPOINT ["/entrypoint.sh"]
