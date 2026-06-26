//! Home Assistant deployment lifecycle tool surface.
//!
//! Net-new over the API/registry surface: these `#[orca_tool]`s own the full
//! deploy lifecycle of a Home Assistant instance — provision, version bump, and
//! config backup/restore — driving the host's container runtime (`pct` for
//! Proxmox LXC, `docker` for Compose) and `tar` for the `/config` volume through
//! `tokio::process::Command`. There is no parallel shell glue: the bootstrap
//! scripts in `scripts/` + `lxc/` are the curl-bootstrap payload these tools
//! orchestrate, and every capability is reachable as an orca tool.
//!
//! Imports flow through `plugin_toolkit::prelude::*` only — the toolkit is the
//! single gateway. Process exec uses the toolkit's re-exported `tokio`.
#![allow(clippy::disallowed_types)]

use std::path::Path;
use std::process::Output;

use plugin_toolkit::prelude::*;
use plugin_toolkit::tokio::process::Command;

/// Where a Home Assistant instance is deployed — selects which runtime the
/// lifecycle tools drive.
#[derive(
    Debug,
    Clone,
    Copy,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
    plugin_toolkit::clap::ValueEnum,
    Default,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Docker / Compose, driven via `docker`.
    #[default]
    Docker,
    /// Proxmox LXC, driven via `pct`.
    Lxc,
}

/// Release channel for `home-assistant.update`. Maps to a container image tag.
#[derive(
    Clone,
    Copy,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
    plugin_toolkit::clap::ValueEnum,
    Default,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Newest stable Home Assistant release.
    #[default]
    Stable,
    /// Beta / release-candidate channel.
    Beta,
    /// Bleeding-edge nightly dev channel.
    Dev,
}

impl Channel {
    /// The container image tag this channel resolves to. Home Assistant
    /// publishes `stable`, `beta`, and `dev` tags on
    /// `ghcr.io/home-assistant/home-assistant`.
    fn image_tag(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Dev => "dev",
        }
    }
}

/// Run a command, capturing output, and map a non-zero exit to an error that
/// carries stderr — the lifecycle tools surface the runtime's own message
/// rather than a bare exit code.
async fn run(cmd: &mut Command) -> Result<Output> {
    let output = cmd
        .output()
        .await
        .with_context(|| "failed to spawn command".to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("command failed ({}): {}", output.status, stderr.trim());
    }
    Ok(output)
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.install — provision a Docker or LXC deployment
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct HaInstallArgs {
    /// Where to deploy: `docker` (Compose) or `lxc` (Proxmox).
    #[arg(long, value_enum, default_value_t = Runtime::Docker)]
    #[serde(default)]
    pub runtime: Runtime,
    /// LXC vmid (LXC runtime only). Required when `runtime=lxc`.
    #[arg(long)]
    #[serde(default)]
    pub vmid: Option<u32>,
    /// Host path for the persistent `/config` volume.
    #[arg(long, default_value = "/opt/homeassistant/config")]
    #[serde(default = "default_config_path")]
    pub config_path: String,
    /// Path to the bootstrap `provision.sh` (LXC) or `compose.yml` (Docker).
    /// Defaults to the repo-relative asset; override for a non-standard layout.
    #[arg(long)]
    #[serde(default)]
    pub bootstrap_path: Option<String>,
}

fn default_config_path() -> String {
    "/opt/homeassistant/config".to_string()
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct HaInstallOutput {
    /// True when the provisioning command completed successfully.
    pub provisioned: bool,
    /// The runtime the deployment targeted.
    pub runtime: Runtime,
    /// Combined stdout from the provisioning step.
    pub log: String,
}

/// **Provision a Home Assistant deployment.** On `docker`, brings up the
/// Compose stack (host networking + persistent `/config`). On `lxc`, runs the
/// Proxmox `provision.sh` bootstrap (create CT, install, start).
#[orca_tool(domain = "home-assistant", verb = "install")]
async fn ha_install(args: HaInstallArgs, _ctx: &ToolCtx) -> Result<HaInstallOutput> {
    let output = match args.runtime {
        Runtime::Docker => {
            let compose = args
                .bootstrap_path
                .clone()
                .unwrap_or_else(|| "compose.yml".to_string());
            let mut cmd = Command::new("docker");
            cmd.arg("compose").arg("-f").arg(&compose).arg("up").arg("-d");
            run(&mut cmd).await?
        }
        Runtime::Lxc => {
            let vmid = args.vmid.context("`vmid` is required when runtime=lxc")?;
            let script = args
                .bootstrap_path
                .clone()
                .unwrap_or_else(|| "lxc/provision.sh".to_string());
            let mut cmd = Command::new("bash");
            cmd.arg(&script).arg(vmid.to_string());
            cmd.arg("--config").arg(&args.config_path);
            run(&mut cmd).await?
        }
    };
    Ok(HaInstallOutput {
        provisioned: true,
        runtime: args.runtime,
        log: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.update — channel-aware image/version bump
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct HaUpdateArgs {
    /// Where the instance runs: `docker` or `lxc`.
    #[arg(long, value_enum, default_value_t = Runtime::Docker)]
    #[serde(default)]
    pub runtime: Runtime,
    /// Release channel to move to. `stable` / `beta` / `dev`.
    #[arg(long, value_enum, default_value_t = Channel::Stable)]
    #[serde(default)]
    pub channel: Channel,
    /// LXC vmid (LXC runtime only).
    #[arg(long)]
    #[serde(default)]
    pub vmid: Option<u32>,
    /// Compose file (Docker runtime only).
    #[arg(long, default_value = "compose.yml")]
    #[serde(default = "default_compose")]
    pub compose_file: String,
}

fn default_compose() -> String {
    "compose.yml".to_string()
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct HaUpdateOutput {
    /// True when the update command completed.
    pub updated: bool,
    /// Image tag the channel resolved to.
    pub image_tag: String,
    /// Combined stdout from the update step.
    pub log: String,
}

/// **Update a Home Assistant deployment** to the head of a release channel. On
/// `docker`, re-pulls the channel image tag and recreates the container. On
/// `lxc`, pulls the new container image inside the CT and restarts the service.
#[orca_tool(domain = "home-assistant", verb = "update")]
async fn ha_update(args: HaUpdateArgs, _ctx: &ToolCtx) -> Result<HaUpdateOutput> {
    let tag = args.channel.image_tag();
    let image = format!("ghcr.io/home-assistant/home-assistant:{tag}");
    let output = match args.runtime {
        Runtime::Docker => {
            run(Command::new("docker").arg("pull").arg(&image)).await?;
            run(Command::new("docker")
                .arg("compose")
                .arg("-f")
                .arg(&args.compose_file)
                .arg("up")
                .arg("-d"))
            .await?
        }
        Runtime::Lxc => {
            let vmid = args.vmid.context("`vmid` is required when runtime=lxc")?;
            run(Command::new("pct")
                .arg("exec")
                .arg(vmid.to_string())
                .arg("--")
                .arg("bash")
                .arg("-c")
                .arg(format!(
                    "docker pull {image} && docker compose -f /opt/homeassistant/compose.yml up -d"
                )))
            .await?
        }
    };
    Ok(HaUpdateOutput {
        updated: true,
        image_tag: tag.to_string(),
        log: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.backup — tar the /config volume to a destination
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct HaBackupArgs {
    /// Host path of the Home Assistant `/config` volume to archive.
    #[arg(long, default_value = "/opt/homeassistant/config")]
    #[serde(default = "default_config_path")]
    pub config_path: String,
    /// Directory to write the `.tar.gz` into. Created if missing.
    #[arg(long)]
    pub destination: String,
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct HaBackupOutput {
    /// Absolute path of the archive written.
    pub archive: String,
}

/// **Back up the Home Assistant `/config` volume** to a `.tar.gz` in the
/// destination directory. The regenerable `home-assistant.log`, `deps/`, and
/// `tts/` trees are excluded — only durable config / `.storage` / db is
/// archived.
#[orca_tool(domain = "home-assistant", verb = "backup")]
async fn ha_backup(args: HaBackupArgs, _ctx: &ToolCtx) -> Result<HaBackupOutput> {
    backup_config(&args).await
}

/// Archive logic, independent of the tool context so it is directly testable.
async fn backup_config(args: &HaBackupArgs) -> Result<HaBackupOutput> {
    let config = Path::new(&args.config_path);
    if !config.is_dir() {
        bail!("config path '{}' is not a directory", args.config_path);
    }
    run(Command::new("mkdir").arg("-p").arg(&args.destination)).await?;

    let stamp = now_stamp();
    let archive = format!(
        "{}/homeassistant-config-{}.tar.gz",
        args.destination.trim_end_matches('/'),
        stamp
    );

    run(Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("--exclude=./deps")
        .arg("--exclude=./tts")
        .arg("--exclude=./home-assistant.log")
        .arg("-C")
        .arg(&args.config_path)
        .arg("."))
    .await?;

    Ok(HaBackupOutput { archive })
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.restore — restore the /config volume from a tarball
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct HaRestoreArgs {
    /// The backup tarball to restore from.
    #[arg(long = "from")]
    pub from: String,
    /// Host path of the `/config` volume to restore into. Created if missing.
    #[arg(long, default_value = "/opt/homeassistant/config")]
    #[serde(default = "default_config_path")]
    pub config_path: String,
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct HaRestoreOutput {
    /// True when extraction completed.
    pub restored: bool,
    /// Where the config was restored to.
    pub config_path: String,
}

/// **Restore the Home Assistant `/config` volume** from a `.tar.gz` produced by
/// `home-assistant.backup`. The service should be stopped before restoring;
/// this tool only extracts the archive over the config directory.
#[orca_tool(domain = "home-assistant", verb = "restore")]
async fn ha_restore(args: HaRestoreArgs, _ctx: &ToolCtx) -> Result<HaRestoreOutput> {
    restore_config(args).await
}

/// Extraction logic, independent of the tool context so it is directly testable.
async fn restore_config(args: HaRestoreArgs) -> Result<HaRestoreOutput> {
    if !Path::new(&args.from).is_file() {
        bail!("backup tarball '{}' not found", args.from);
    }
    run(Command::new("mkdir").arg("-p").arg(&args.config_path)).await?;
    run(Command::new("tar")
        .arg("-xzf")
        .arg(&args.from)
        .arg("-C")
        .arg(&args.config_path))
    .await?;
    Ok(HaRestoreOutput {
        restored: true,
        config_path: args.config_path,
    })
}

/// UTC timestamp `YYYYMMDD-HHMMSS` for archive names. chrono is reached through
/// the toolkit re-export so the plugin carries no direct chrono dep.
fn now_stamp() -> String {
    plugin_toolkit::chrono::Utc::now()
        .format("%Y%m%d-%H%M%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_maps_to_image_tag() {
        assert_eq!(Channel::Stable.image_tag(), "stable");
        assert_eq!(Channel::Beta.image_tag(), "beta");
        assert_eq!(Channel::Dev.image_tag(), "dev");
    }

    #[tokio::test]
    async fn backup_rejects_missing_config_dir() {
        let args = HaBackupArgs {
            config_path: "/nonexistent/homeassistant/config/path".to_string(),
            destination: "/tmp/homeassistant-test-dest".to_string(),
        };
        let err = backup_config(&args).await.unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[tokio::test]
    async fn restore_rejects_missing_tarball() {
        let args = HaRestoreArgs {
            from: "/nonexistent/backup.tar.gz".to_string(),
            config_path: "/tmp/homeassistant-test-restore".to_string(),
        };
        let err = restore_config(args).await.unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn backup_then_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        std::fs::create_dir_all(config.join(".storage")).unwrap();
        std::fs::write(config.join("configuration.yaml"), b"default_config:").unwrap();
        std::fs::write(config.join(".storage").join("core.config"), b"{}").unwrap();
        // deps must be excluded
        std::fs::create_dir_all(config.join("deps")).unwrap();
        std::fs::write(config.join("deps").join("junk"), b"x").unwrap();

        let dest = tmp.path().join("backups");
        let out = backup_config(&HaBackupArgs {
            config_path: config.to_string_lossy().into_owned(),
            destination: dest.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        assert!(Path::new(&out.archive).is_file());

        let restore_target = tmp.path().join("restored");
        restore_config(HaRestoreArgs {
            from: out.archive.clone(),
            config_path: restore_target.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

        assert!(restore_target.join("configuration.yaml").is_file());
        assert!(restore_target.join(".storage").join("core.config").is_file());
        // deps was excluded from the archive
        assert!(!restore_target.join("deps").exists());
    }
}
