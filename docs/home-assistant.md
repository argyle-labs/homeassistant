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

## Extra mounts

`home-assistant.install --mount HOST:CONTAINER[:ro|:rw]` (repeatable) binds
host paths such as a media share or a backup target into the container.
Host paths must sit under `/mnt/`, `/media/`, `/srv/` or `/data/`; paths may
only use `A-Z a-z 0-9 . _ / -`. `/config`, `/etc/localtime`, and `/run/dbus` are
reserved targets, and two mounts may not share a target. `install` is dry-run unless `--execute` is passed; the dry
run validates the mounts and returns the planned steps. `install` and
`upgrade` require the `admin` role, for dry runs too.

- **Docker:** mounts are written to `compose.mounts.yml` beside `compose.yml`
  and passed as a second `-f`. `home-assistant.upgrade` includes that override
  whenever it exists, so upgrades keep the mounts.
- **LXC:** `provision.sh --mount` adds `mp1`, `mp2`, … to the CT (`mp0` is
  `/config`). Each host path appears at the same container path inside both the
  CT and the HA container. Invalid mounts are rejected before `pct create`.

## Backup & restore

The `/config` volume is the whole service state. For a clean copy, stop the
container first:

```sh
docker compose stop
f=ha-config-$(date +%Y%m%d).tar.gz
tar -czf "$f" \
  --exclude=./deps --exclude=./tts --exclude=./home-assistant.log \
  -C /opt/homeassistant/config .
sha256sum "$f" > "$f.sha256"
docker compose start
```

`deps/`, `tts/`, and `home-assistant.log` are regenerable and excluded. Restore
by extracting the archive back over `/config` (with the container stopped) and
starting it again; run `sha256sum -c "$f.sha256"` first and do not restore an
archive that fails it.

With orca, `home-assistant.backup` / `home-assistant.restore` perform exactly
this. Both are dry-run unless `--execute` is passed, and both require the
`admin` role, for dry runs too. `backup` writes a
`<archive>.sha256` sidecar and re-verifies it; `restore` verifies the archive
against that sidecar (on dry runs too) and refuses a missing, malformed, or
mismatched checksum. Restore copies the archive into a private 0700
directory, verifies that copy, and extracts the copy, so the file checked is
the file restored. The sidecar detects corruption and truncation; it does not
authenticate, because anyone who can rewrite the archive can rewrite the
sidecar beside it. Home Assistant's own **Settings → System → Backups** UI is
also available for in-app snapshots.

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
