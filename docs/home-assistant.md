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

## Voice naming audit

`home-assistant.voice-audit` reads `/api/states` and reports controllable
entities (lights, switches, locks, media players, scenes, …) whose names voice
assistants handle badly. It is read-only and calls no service.

| Issue | Meaning |
|---|---|
| `missing_name` | No friendly name; voice falls back to the entity id. |
| `ambiguous` | A bare device word such as "Speaker" or "Overhead". |
| `duplicate` | Same name (case-insensitive) as another controllable entity. |
| `leaked_slug` | Underscore or serial number in the name ("googlehome7284"). |
| `power_critical` | A controllable entity whose name or entity id marks servers, network gear, or the media center; keep it out of `conversation` exposure. |

Aliases and voice exposure live in the entity registry (WebSocket API), which
this REST-based audit does not read; it flags names only.

## WebSocket API and registry

Registry, exposure, assist, backup, and Supervisor data are only on Home
Assistant's WebSocket API (`/api/websocket`). The plugin's WebSocket client
(`src/ws.rs`) handles the auth handshake and request/result matching. It
needs a WebSocket transport from `plugin-toolkit`, which does not exist yet;
until it lands, WebSocket-backed tools return "WebSocket transport
unavailable".

| Tool | Mutates | Purpose |
|---|---|---|
| `home-assistant.registry-entities` | no | Every entity with name, aliases, area, device, hidden/disabled, platform. |
| `home-assistant.registry-areas` | no | Areas with aliases and floor. |
| `home-assistant.registry-update` | yes | Set name, aliases, or area on one entity (an empty `--name` or `--area-id` clears it). Admin only (dry runs too). Executes by default; `--dry-run` previews; each write is confirmed by re-reading the entity. |

Registry traps the client handles: `entity_registry/list` omits aliases (the
client reads `get_entries`, falling back to `get`); `aliases: [null]` means
none; a dropped connection can lose a write while returning a falsy result.

## Supervisor mounts

`home-assistant.mount-*` manages the Supervisor's SMB/NFS mounts through the
WebSocket `supervisor/api` passthrough. A long-lived token gets 401 on the
REST `/api/hassio/*` proxy. The plugin controls the Supervisor; it never
mounts anything itself.

| Tool | Mutates | Purpose |
|---|---|---|
| `mount-list`, `mount-detail` | no | Mounts with state, plus `default_backup_mount`. |
| `mount-health` | no | Probes each server's service ports (NFS 2049, SMB 445) and classifies `healthy`, `stale_session` (server up: run `mount-reload`), `server_down`, or `unknown`. Alerts on a missing default backup mount and on no active backup mount. |
| `mount-create`, `mount-update` | yes | CIFS username/password come from orca secrets (`home-assistant.<name>.username` / `.password`, or `--username-secret` / `--password-secret`); the plan shows only `<secret:NAME>`. |
| `mount-delete` | yes | Refuses the default backup mount without `--allow-default`. |
| `mount-reload` | yes | Discards a stale session and mounts fresh. |
| `mount-options` | yes | Sets `default_backup_mount` (must be an existing backup mount). |

Every mutating verb requires the `admin` role (dry runs too), is dry-run unless
`--execute`, and returns the re-read mount list after an executed call. A dry
run of `mount-create`/`mount-update` fails if a referenced secret does not
exist; `mount-update` fails if any requested field did not read back. A mount path that
is a subdirectory of the server's export is legal and is not flagged.
