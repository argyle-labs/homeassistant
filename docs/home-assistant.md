# Home Assistant OS

Home automation hub. Runs as a VM on a Proxmox host.

**Status:** running — VM on `<ip>:8123`

---

## Instance

| Field | Value |
|---|---|
| VM ID | 105 |
| Host | <host> (<ip>) |
| IP | <ip> (static DHCP lease) |
| OS | Home Assistant OS |
| CPU | 2 cores |
| RAM | 4 GB |
| Disk | 32 GB (local-lvm, SSD) |
| Machine | q35, OVMF BIOS |
| onboot | yes |
| Web UI | http://<ip>:8123 |

---

## Integrations

| Integration | LXC/VM | IP | Notes |
|---|---|---|---|
| Mosquitto MQTT | LXC | <ip> | Message broker — Z-Wave, Zigbee, other MQTT devices |
| Zigbee2MQTT | LXC | <ip> | Zigbee coordinator, bridges to MQTT |
| Z-Wave JS UI | LXC | <ip> | Z-Wave controller, bridges to MQTT |
| AdGuard Home | LXC | <ip> | DNS — HA uses for local name resolution |
| UniFi | LXC | <ip> | WiFi presence detection |

---

## Service Management

HAOS is managed entirely through the web UI. SSH access is available via the terminal add-on or developer tools.

```bash
# Direct SSH (if terminal add-on enabled)
ssh root@<ip>

# From Proxmox host (this is a VM, not LXC)
qm terminal 105
```

---

## Backup

HAOS has a built-in backup system: **Settings → System → Backups → Create Backup**

Backups should be pushed to your NAS. Configure the Samba/NFS backup target:
- Settings → System → Backups → change backup location to a network share

> **TODO:** Configure automatic backup to a network share (e.g. `/mnt/user/backups/homeassistant/`).

---

## Planned: Move to IoT VLAN

Home Assistant and Z-Wave JS UI can be moved to an IoT VLAN with firewall rules allowing LAN access on ports 8123 and 8091. See your OPNsense setup (IoT VLAN section).

---

## Related

- MQTT broker
- Zigbee2MQTT integration
- Z-Wave JS UI integration
