# Home Assistant — operator notes

Operator notes for running the container image this repo ships. For the plugin
tool surface and standalone quick-start, see the [README](../README.md).

## Deployment

Home Assistant runs from [`compose.yml`](../compose.yml):

- **Image:** `ghcr.io/home-assistant/home-assistant:stable`
- **Networking:** `network_mode: host` — the web UI is served on port **8123**.
- **State:** `/opt/homeassistant/config` on the host, mapped to `/config` in the
  container. This directory (config YAML, `.storage/`, the SQLite recorder db) is
  the entire durable state.
- **Privileged:** enabled for USB / Bluetooth (Zigbee / Z-Wave) passthrough. Drop
  `privileged` and the `/run/dbus` mount if you don't attach local radios.

Bring it up:

```sh
docker compose up -d          # or: podman compose -f compose.yml up -d
docker logs -f homeassistant  # watch first-run onboarding
```

Then open `http://<host>:8123` and complete onboarding.

Alternative runtimes: Proxmox LXC via [`lxc/provision.sh`](../lxc/provision.sh);
Compose variants for Bluetooth / Zigbee live in [`examples/`](../examples/).

## Updating

Move to the head of a release channel by re-pulling the image tag and recreating
the container:

```sh
docker compose pull && docker compose up -d
```

Home Assistant publishes `stable`, `beta`, and `dev` tags on
`ghcr.io/home-assistant/home-assistant`. With orca, `home-assistant.update`
(`--channel stable|beta|dev`) does this for Docker or LXC.

## Backup & restore

The `/config` volume is the whole service state. For a clean copy, stop the
container first:

```sh
docker compose stop
tar -czf ha-config-$(date +%Y%m%d).tar.gz \
  --exclude=./deps --exclude=./tts --exclude=./home-assistant.log \
  -C /opt/homeassistant/config .
docker compose start
```

`deps/`, `tts/`, and `home-assistant.log` are regenerable and excluded. Restore
by extracting the archive back over `/config` (with the container stopped) and
starting it again. With orca, `home-assistant.backup` / `home-assistant.restore`
perform exactly this, and Home Assistant's own **Settings → System → Backups**
UI is available for in-app snapshots.

## Configuring an orca endpoint

To drive a running instance through orca's `home-assistant.*` tools, register it
with a long-lived access token (HA profile → Security → Long-lived access
tokens):

```sh
orca home-assistant.create --name home --base-url http://<host>:8123 --token <token>
orca home-assistant.entities --domain light
orca home-assistant.service --service-domain light --service-name turn_on --entity-id light.example
```

The token is stored as a secret. See the [README](../README.md) for the full
tool list.
