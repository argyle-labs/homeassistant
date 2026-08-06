<p align="center">
  <img src="assets/icon-256.png" width="120" alt="homeassistant" />
</p>

# homeassistant

Adapts [Home Assistant](https://www.home-assistant.io/) — the open-source home-automation platform — into [orca](https://github.com/argyle-labs/orca) as a tool-surface plugin: register HA endpoints, read entities / automations, invoke services, and drive the full deploy lifecycle (install / update / backup / restore) on Docker or Proxmox LXC.

A first-party orca plugin (service adapter). It talks to Home Assistant over its REST API and manages the deployment through the host's container runtime — there is no `ServiceBackend` / `service.*` surface here; every capability is a dedicated `home-assistant.*` tool.

Everything works **two ways**:

- **With orca** — call the `home-assistant.*` tools and orca runs the right thing on the host.
- **Without orca (standalone)** — run Home Assistant straight from the shipped [`compose.yml`](compose.yml).

---

## Run it without orca (standalone)

The repo ships a `compose.yml` that runs the upstream image directly.

```sh
docker compose up -d
```

The stack ([`compose.yml`](compose.yml)) runs `ghcr.io/home-assistant/home-assistant:stable` with `network_mode: host` (the web UI is served on port **8123**), persists state in `/opt/homeassistant/config` mapped to the container's `/config`, and mounts `/etc/localtime` and `/run/dbus` (host D-Bus, for Bluetooth integrations). It is `privileged` for USB/Bluetooth (Zigbee/Z-Wave) passthrough — drop that if you don't need local radios.

See [`examples/`](examples/) for `docker-compose.basic.yml`, `docker-compose.bluetooth.yml`, and `docker-compose.zigbee.yml` variants, and [`lxc/`](lxc/) for the Proxmox LXC bootstrap.

### Other runtimes

**Podman**: `podman compose -f compose.yml up -d`. **Proxmox LXC**: use [`lxc/provision.sh`](lxc/provision.sh). **Unraid**: *Docker → Add Container* with the image / port `8123` / `/config` volume from `compose.yml`. Upstream: <https://www.home-assistant.io/>.

### Backup & restore

Back up the `/config` volume — that is the whole service state (stop the container first for a clean copy). Restore by putting it back and starting the container. With orca, `home-assistant.backup` / `home-assistant.restore` do exactly this (excluding regenerable `deps/`, `tts/`, and `home-assistant.log`). See [`docs/home-assistant.md`](docs/home-assistant.md) for operator notes.

---

## With orca

Register an endpoint, then read/drive Home Assistant through typed tools. Two families:

### API + endpoint registry — `src/tools.rs`

| tool | what it does | key args |
| --- | --- | --- |
| `home-assistant.create` | register an HA endpoint | `name`, `base_url`, `token` (secret), `enabled` |
| `home-assistant.list` | list registered endpoints | — |
| `home-assistant.detail` | inspect one endpoint | `name` |
| `home-assistant.update` | edit a registered endpoint | `name`, fields to change |
| `home-assistant.delete` | remove an endpoint | `name` |
| `home-assistant.entities` | list entities, optionally domain-filtered | optional `domain` |
| `home-assistant.entity` | single entity's current state | `entity_id` |
| `home-assistant.automations` | list configured automations | — |
| `home-assistant.service` | **[mutates]** invoke an HA service (admin) | `service_domain`, `service_name`, optional `entity_id`, `service_data` |

The `list` / `detail` / `create` / `update` / `delete` CRUD is generated wholesale by `#[endpoint_resource]`; `token` is stored as a secret. `entities` / `entity` / `automations` / `service` are hand-written endpoint tools that resolve the registered endpoint's client and call the HA REST API.

### Deploy lifecycle — `src/lifecycle.rs`

| tool | what it does | key args |
| --- | --- | --- |
| `home-assistant.install` | provision a deployment | `runtime` (`docker`\|`lxc`), `config_path`, `vmid` (lxc), `bootstrap_path` |
| `home-assistant.update` | bump to a release channel's image | `runtime`, `channel` (`stable`\|`beta`\|`dev`), `vmid`, `compose_file` |
| `home-assistant.backup` | tar the `/config` volume to a `.tar.gz` | `config_path`, `destination` |
| `home-assistant.restore` | restore `/config` from a tarball | `from`, `config_path` |

`install` / `update` drive the host runtime directly — `docker compose` for Docker, `pct` for Proxmox LXC — using the repo's `compose.yml` / `lxc/provision.sh` as the bootstrap payload. `backup` / `restore` tar the `/config` volume, excluding the regenerable `deps/`, `tts/`, and `home-assistant.log`.

---

## Layout

- `src/lib.rs` — the HA REST client (`Config`, `Client`, `ServiceCall`) and error type.
- `src/tools.rs` — the `#[endpoint_resource]` registry CRUD plus the `entities` / `entity` / `automations` / `service` endpoint tools.
- `src/lifecycle.rs` — the `home-assistant.{install,update,backup,restore}` deploy-lifecycle tools.
- `src/main.rs` — plugin entrypoint.
- `compose.yml` — standalone Docker deployment.
- `examples/` — Compose variants (basic / bluetooth / zigbee).
- `lxc/` — Proxmox LXC bootstrap (`provision.sh`, `homeassistant.conf.example`).
- `scripts/` — provisioning / lifecycle helpers (`install.sh`, `backup.sh`, `restore.sh`, `configure.sh`, `entrypoint.sh`).
- `docs/` — operator notes.
- `assets/` — plugin icon.
