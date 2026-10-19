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

use plugin_toolkit::prelude::*;
use plugin_toolkit::process::{Command, Output};

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
async fn run(cmd: Command) -> Result<Output> {
    let output = cmd
        .output()
        .await
        .with_context(|| "failed to spawn command".to_string())?;
    if !output.status.success {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        bail!("command failed (exit {}): {}", code, stderr.trim());
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
    /// Extra bind mount `HOST:CONTAINER[:ro|:rw]`; repeatable. `/config`,
    /// `/etc/localtime`, and `/run/dbus` are reserved.
    #[arg(long = "mount")]
    #[serde(default)]
    pub mounts: Vec<String>,
    /// Validate and return the plan without provisioning.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
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
    /// True when nothing was run (`dry_run`).
    pub dry_run: bool,
    /// True when the provisioning command completed successfully.
    pub provisioned: bool,
    /// The runtime the deployment targeted.
    pub runtime: Runtime,
    /// Validated extra bind mounts.
    pub mounts: Vec<crate::mounts::BindMount>,
    /// Steps run (or that would run), in order.
    pub plan: Vec<String>,
    /// Combined stdout from the provisioning step.
    pub log: String,
}

/// One install step: write a file, or run an argv.
enum Step {
    Write { path: String, contents: String },
    Run(Vec<String>),
}

impl Step {
    fn describe(&self) -> String {
        match self {
            Step::Write { path, .. } => format!("write {path}"),
            Step::Run(argv) => argv.join(" "),
        }
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn install_plan(args: &HaInstallArgs) -> Result<(Vec<crate::mounts::BindMount>, Vec<Step>)> {
    let mounts = crate::mounts::parse_all(&args.mounts)?;
    let steps = match args.runtime {
        Runtime::Docker => {
            let compose = args.bootstrap_path.as_deref().unwrap_or("compose.yml");
            let mut up = argv(&["docker", "compose", "-f", compose]);
            let mut steps = Vec::new();
            if !mounts.is_empty() {
                let path = crate::mounts::override_path(compose);
                up.extend(argv(&["-f", &path]));
                steps.push(Step::Write {
                    path,
                    contents: crate::mounts::render_override(&mounts),
                });
            }
            up.extend(argv(&["up", "-d"]));
            steps.push(Step::Run(up));
            steps
        }
        Runtime::Lxc => {
            let vmid = args.vmid.context("`vmid` is required when runtime=lxc")?;
            let script = args.bootstrap_path.as_deref().unwrap_or("lxc/provision.sh");
            let mut cmd = argv(&[
                "bash",
                script,
                &vmid.to_string(),
                "--config",
                &args.config_path,
            ]);
            for spec in &args.mounts {
                cmd.extend(argv(&["--mount", spec]));
            }
            vec![Step::Run(cmd)]
        }
    };
    Ok((mounts, steps))
}

/// **Provision a Home Assistant deployment.** On `docker`, brings up the
/// Compose stack (host networking + persistent `/config`). On `lxc`, runs the
/// Proxmox `provision.sh` bootstrap (create CT, install, start).
#[orca_tool(domain = "home-assistant", verb = "install", role = "admin")]
async fn ha_install(args: HaInstallArgs, ctx: &ToolCtx) -> Result<HaInstallOutput> {
    crate::authz::require_admin(ctx)?;
    install(&args).await
}

async fn install(args: &HaInstallArgs) -> Result<HaInstallOutput> {
    let (mounts, steps) = install_plan(args)?;
    let plan = steps.iter().map(Step::describe).collect();
    if args.dry_run {
        return Ok(HaInstallOutput {
            dry_run: true,
            provisioned: false,
            runtime: args.runtime,
            mounts,
            plan,
            log: String::new(),
        });
    }
    let mut log = String::new();
    for step in &steps {
        match step {
            Step::Write { path, contents } => std::fs::write(path, contents)
                .with_context(|| format!("failed to write '{path}'"))?,
            Step::Run(cmd) => {
                let out = run(Command::new(&cmd[0]).args(&cmd[1..])).await?;
                log.push_str(&String::from_utf8_lossy(&out.stdout));
            }
        }
    }
    Ok(HaInstallOutput {
        dry_run: false,
        provisioned: true,
        runtime: args.runtime,
        mounts,
        plan,
        log,
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
    /// Return the planned commands without running them.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
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
    /// True when nothing was run (`dry_run`).
    pub dry_run: bool,
    /// True when the update command completed.
    pub updated: bool,
    /// Commands run (or that would run), in order.
    pub plan: Vec<String>,
    /// Image tag the channel resolved to.
    pub image_tag: String,
    /// Combined stdout from the update step.
    pub log: String,
}

/// **Upgrade a Home Assistant deployment** to the head of a release channel. On
/// `docker`, re-pulls the channel image tag and recreates the container. On
/// `lxc`, pulls the new container image inside the CT and restarts the service.
/// Executes unless `dry_run` is set.
///
/// Distinct from `home-assistant.update`, which edits endpoint registry metadata
/// (base_url/token/enabled) and never touches the deployment.
#[orca_tool(domain = "home-assistant", verb = "upgrade", role = "admin")]
async fn ha_upgrade(args: HaUpdateArgs, ctx: &ToolCtx) -> Result<HaUpdateOutput> {
    crate::authz::require_admin(ctx)?;
    upgrade(&args).await
}

fn upgrade_plan(args: &HaUpdateArgs) -> Result<Vec<Vec<String>>> {
    let image = format!(
        "ghcr.io/home-assistant/home-assistant:{}",
        args.channel.image_tag()
    );
    let argv = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    Ok(match args.runtime {
        Runtime::Docker => {
            let mut up = argv(&["docker", "compose", "-f", &args.compose_file]);
            // Recreating without the override would silently drop the extra mounts.
            let overlay = crate::mounts::override_path(&args.compose_file);
            if Path::new(&overlay).is_file() {
                up.extend(argv(&["-f", &overlay]));
            }
            up.extend(argv(&["up", "-d"]));
            vec![argv(&["docker", "pull", &image]), up]
        }
        Runtime::Lxc => {
            let vmid = args.vmid.context("`vmid` is required when runtime=lxc")?;
            vec![argv(&[
                "pct",
                "exec",
                &vmid.to_string(),
                "--",
                "bash",
                "-c",
                &format!(
                    "docker pull {image} && cd /opt/homeassistant && \
                     docker compose -f compose.yml $([ -f {o} ] && echo -f {o}) up -d",
                    o = crate::mounts::OVERRIDE_FILE
                ),
            ])]
        }
    })
}

async fn upgrade(args: &HaUpdateArgs) -> Result<HaUpdateOutput> {
    let steps = upgrade_plan(args)?;
    let plan = steps.iter().map(|a| a.join(" ")).collect();
    let image_tag = args.channel.image_tag().to_string();
    if args.dry_run {
        return Ok(HaUpdateOutput {
            dry_run: true,
            updated: false,
            plan,
            image_tag,
            log: String::new(),
        });
    }
    let mut log = String::new();
    for argv in &steps {
        let out = run(Command::new(&argv[0]).args(&argv[1..])).await?;
        log.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    Ok(HaUpdateOutput {
        dry_run: false,
        updated: true,
        plan,
        image_tag,
        log,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.backup — tar the /config volume to a destination
// ═══════════════════════════════════════════════════════════════════════════

/// Suffix of the checksum sidecar written next to every archive.
const CHECKSUM_SUFFIX: &str = ".sha256";

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
    /// Report the planned archive without writing it.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
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
    /// True when nothing was written (`dry_run`).
    pub dry_run: bool,
    /// Absolute path of the archive written (or that would be written).
    pub archive: String,
    /// Path of the `sha256sum`-format sidecar.
    pub checksum_path: String,
    /// Hex SHA-256 of the archive; `None` on a dry run.
    pub sha256: Option<String>,
}

/// **Back up the Home Assistant `/config` volume** to a `.tar.gz` in the
/// destination directory, plus a `.sha256` sidecar that `restore` verifies.
/// The regenerable `home-assistant.log`, `deps/`, and `tts/` trees are
/// excluded. Executes unless `dry_run` is set.
#[orca_tool(domain = "home-assistant", verb = "backup", role = "admin")]
async fn ha_backup(args: HaBackupArgs, ctx: &ToolCtx) -> Result<HaBackupOutput> {
    crate::authz::require_admin(ctx)?;
    backup_config(&args).await
}

/// Archive logic, independent of the tool context so it is directly testable.
async fn backup_config(args: &HaBackupArgs) -> Result<HaBackupOutput> {
    let config = Path::new(&args.config_path);
    if !config.is_dir() {
        bail!("config path '{}' is not a directory", args.config_path);
    }

    let archive = format!(
        "{}/homeassistant-config-{}.tar.gz",
        args.destination.trim_end_matches('/'),
        now_stamp()
    );
    let checksum_path = format!("{archive}{CHECKSUM_SUFFIX}");
    if args.dry_run {
        return Ok(HaBackupOutput {
            dry_run: true,
            archive,
            checksum_path,
            sha256: None,
        });
    }

    run(Command::new("mkdir").arg("-p").arg(&args.destination)).await?;
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

    let sha256 = hash_file(&archive)?;
    write_sidecar(&archive, &checksum_path, &sha256)?;

    Ok(HaBackupOutput {
        dry_run: false,
        archive,
        checksum_path,
        sha256: Some(sha256),
    })
}

fn hash_file(path: &str) -> Result<String> {
    plugin_toolkit::hash::sha256_file(Path::new(path))
        .with_context(|| format!("failed to hash '{path}'"))
}

/// Writes `<hex>  <basename>` (so `sha256sum -c` also checks it) via a
/// sibling temp file and rename, so a reader never sees a partial sidecar.
fn write_sidecar(archive: &str, checksum_path: &str, sha256: &str) -> Result<()> {
    let name = Path::new(archive)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| archive.to_string());
    let tmp = format!("{checksum_path}.tmp.{}", std::process::id());
    std::fs::write(&tmp, format!("{sha256}  {name}\n"))
        .with_context(|| format!("failed to write checksum '{tmp}'"))?;
    std::fs::rename(&tmp, checksum_path)
        .with_context(|| format!("failed to write checksum '{checksum_path}'"))
}

/// A copy of an archive in a fresh 0700 directory, removed on drop. Restore
/// hashes and extracts this copy, so the file checked is the file extracted
/// even if the original is replaced in between.
struct PrivateCopy {
    dir: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl Drop for PrivateCopy {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            plugin_toolkit::tracing::warn!("failed to remove '{}': {e}", self.dir.display());
        }
    }
}

fn private_copy(archive: &str) -> Result<PrivateCopy> {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ha-restore-{}-{nanos}-{seq}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("failed to create '{}'", dir.display()))?;
    let copy = PrivateCopy {
        file: dir.join("archive.tar.gz"),
        dir,
    };
    std::fs::copy(archive, &copy.file).with_context(|| format!("failed to copy '{archive}'"))?;
    Ok(copy)
}

/// Copies `archive` privately, verifies the copy against the sidecar, and
/// returns the digest with the verified copy. A missing or malformed sidecar
/// fails: an unverifiable archive is never restored.
///
/// Trust model: the sidecar detects corruption and truncation (bit rot, a
/// short copy, a full disk). It does not authenticate: anyone who can rewrite
/// the archive can rewrite the sidecar beside it.
fn verify_archive(archive: &str) -> Result<(String, PrivateCopy)> {
    let checksum_path = format!("{archive}{CHECKSUM_SUFFIX}");
    let raw = std::fs::read_to_string(&checksum_path).with_context(|| {
        format!("checksum sidecar '{checksum_path}' missing; refusing an unverified archive")
    })?;
    let expected = raw
        .split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .with_context(|| format!("checksum sidecar '{checksum_path}' is malformed"))?
        .to_ascii_lowercase();
    let copy = private_copy(archive)?;
    let actual = hash_file(&copy.file.to_string_lossy())?;
    if actual != expected {
        bail!("checksum mismatch for '{archive}': expected {expected}, got {actual}");
    }
    Ok((actual, copy))
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
    /// Verify the checksum and stop without extracting.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
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
    /// True when nothing was extracted (`dry_run`).
    pub dry_run: bool,
    /// True when extraction completed.
    pub restored: bool,
    /// Hex SHA-256 the archive was verified against.
    pub verified_sha256: String,
    /// Where the config was (or would be) restored to.
    pub config_path: String,
}

/// **Restore the Home Assistant `/config` volume** from a `.tar.gz` produced by
/// `home-assistant.backup`. The archive is checksum-verified against its
/// `.sha256` sidecar first, on dry runs too. The service should be stopped
/// before an executed restore; this tool only extracts over the config dir.
#[orca_tool(domain = "home-assistant", verb = "restore", role = "admin")]
async fn ha_restore(args: HaRestoreArgs, ctx: &ToolCtx) -> Result<HaRestoreOutput> {
    crate::authz::require_admin(ctx)?;
    restore_config(args).await
}

/// Extraction logic, independent of the tool context so it is directly testable.
async fn restore_config(args: HaRestoreArgs) -> Result<HaRestoreOutput> {
    if !Path::new(&args.from).is_file() {
        bail!("backup tarball '{}' not found", args.from);
    }
    let (verified_sha256, copy) = verify_archive(&args.from)?;
    if args.dry_run {
        return Ok(HaRestoreOutput {
            dry_run: true,
            restored: false,
            verified_sha256,
            config_path: args.config_path,
        });
    }
    run(Command::new("mkdir").arg("-p").arg(&args.config_path)).await?;
    run(Command::new("tar")
        .arg("-xzf")
        .arg(&copy.file)
        .arg("-C")
        .arg(&args.config_path))
    .await?;
    Ok(HaRestoreOutput {
        dry_run: false,
        restored: true,
        verified_sha256,
        config_path: args.config_path,
    })
}

/// UTC timestamp `YYYYMMDD-HHMMSS` for archive names. Wall-clock time is reached
/// through the toolkit's `time` seam so the plugin carries no direct chrono dep.
fn now_stamp() -> String {
    plugin_toolkit::time::now().compact()
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

    fn install_args(runtime: Runtime, mounts: &[&str], execute: bool) -> HaInstallArgs {
        HaInstallArgs {
            runtime,
            vmid: Some(120),
            config_path: default_config_path(),
            bootstrap_path: None,
            mounts: mounts.iter().map(|m| m.to_string()).collect(),
            dry_run: !execute,
        }
    }

    fn upgrade_args(runtime: Runtime, vmid: Option<u32>, execute: bool) -> HaUpdateArgs {
        HaUpdateArgs {
            runtime,
            channel: Channel::Beta,
            vmid,
            compose_file: default_compose(),
            dry_run: !execute,
        }
    }

    #[tokio::test]
    async fn install_and_upgrade_refuse_non_admin_even_on_dry_run() {
        use crate::authz::testing::{assert_refused, ctx};
        for c in [ctx(None), ctx(Some("user"))] {
            let err = ha_install(install_args(Runtime::Docker, &[], false), &c)
                .await
                .unwrap_err();
            assert_refused(&err);
            let err = ha_upgrade(upgrade_args(Runtime::Docker, None, false), &c)
                .await
                .unwrap_err();
            assert_refused(&err);
        }
        let out = ha_upgrade(
            upgrade_args(Runtime::Docker, None, false),
            &ctx(Some("admin")),
        )
        .await
        .unwrap();
        assert!(out.dry_run);
    }

    #[tokio::test]
    async fn install_defaults_to_dry_run_plan() {
        let out = install(&install_args(
            Runtime::Docker,
            &["/mnt/media:/media:ro"],
            false,
        ))
        .await
        .unwrap();
        assert!(out.dry_run && !out.provisioned);
        assert_eq!(
            out.plan,
            [
                "write compose.mounts.yml",
                "docker compose -f compose.yml -f compose.mounts.yml up -d"
            ]
        );
        assert!(!Path::new("compose.mounts.yml").exists());
    }

    #[test]
    fn docker_plan_without_mounts_uses_base_compose_only() {
        let (m, steps) = install_plan(&install_args(Runtime::Docker, &[], false)).unwrap();
        assert!(m.is_empty());
        let plan: Vec<_> = steps.iter().map(Step::describe).collect();
        assert_eq!(plan, ["docker compose -f compose.yml up -d"]);
    }

    #[test]
    fn lxc_plan_forwards_mounts_to_provision() {
        let (_, steps) = install_plan(&install_args(
            Runtime::Lxc,
            &["/mnt/media:/media:ro"],
            false,
        ))
        .unwrap();
        let plan: Vec<_> = steps.iter().map(Step::describe).collect();
        assert_eq!(
            plan,
            ["bash lxc/provision.sh 120 --config /opt/homeassistant/config --mount /mnt/media:/media:ro"]
        );
    }

    #[test]
    fn install_rejects_invalid_mount_before_any_step() {
        let err = install_plan(&install_args(Runtime::Docker, &["/mnt/x:/config"], true))
            .err()
            .unwrap();
        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[tokio::test]
    async fn backup_and_restore_refuse_non_admin_even_on_dry_run() {
        use crate::authz::testing::{assert_refused, ctx};
        let tmp = tempfile::tempdir().unwrap();
        let config = fixture_config(tmp.path());
        let dest = tmp.path().join("backups");
        for c in [ctx(None), ctx(Some("user"))] {
            let args = HaBackupArgs {
                config_path: config.to_string_lossy().into_owned(),
                destination: dest.to_string_lossy().into_owned(),
                dry_run: true,
            };
            assert_refused(&ha_backup(args, &c).await.unwrap_err());
            let args = HaRestoreArgs {
                from: "/nonexistent/x.tar.gz".into(),
                config_path: "/nonexistent/c".into(),
                dry_run: true,
            };
            // Refused before the missing archive is even noticed.
            assert_refused(&ha_restore(args, &c).await.unwrap_err());
        }
        let args = HaBackupArgs {
            config_path: config.to_string_lossy().into_owned(),
            destination: dest.to_string_lossy().into_owned(),
            dry_run: true,
        };
        assert!(ha_backup(args, &ctx(Some("admin"))).await.unwrap().dry_run);
    }

    #[tokio::test]
    async fn upgrade_defaults_to_dry_run_plan() {
        let out = upgrade(&upgrade_args(Runtime::Docker, None, false))
            .await
            .unwrap();
        assert!(out.dry_run && !out.updated);
        assert_eq!(out.image_tag, "beta");
        assert_eq!(
            out.plan,
            [
                "docker pull ghcr.io/home-assistant/home-assistant:beta",
                "docker compose -f compose.yml up -d"
            ]
        );
    }

    #[tokio::test]
    async fn upgrade_lxc_plan_targets_the_ct() {
        let out = upgrade(&upgrade_args(Runtime::Lxc, Some(105), false))
            .await
            .unwrap();
        assert_eq!(out.plan.len(), 1);
        assert!(out.plan[0].starts_with("pct exec 105 -- bash -c docker pull "));
    }

    #[tokio::test]
    async fn upgrade_lxc_requires_vmid_even_on_dry_run() {
        let err = upgrade(&upgrade_args(Runtime::Lxc, None, false))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("vmid"), "{err}");
    }

    #[test]
    fn upgrade_keeps_the_mounts_overlay() {
        let tmp = tempfile::tempdir().unwrap();
        let compose = tmp.path().join("compose.yml");
        std::fs::write(&compose, "services: {}").unwrap();
        let mut a = upgrade_args(Runtime::Docker, None, false);
        a.compose_file = compose.to_string_lossy().into_owned();
        let without = upgrade_plan(&a).unwrap();
        assert!(!without[1].iter().any(|x| x.ends_with("compose.mounts.yml")));
        std::fs::write(tmp.path().join("compose.mounts.yml"), "services: {}").unwrap();
        let with = upgrade_plan(&a).unwrap();
        let overlay = tmp
            .path()
            .join("compose.mounts.yml")
            .to_string_lossy()
            .into_owned();
        assert_eq!(&with[1][4..6], ["-f".to_string(), overlay]);
        let lxc = upgrade_plan(&upgrade_args(Runtime::Lxc, Some(105), false)).unwrap();
        assert!(lxc[0][6].contains("compose.mounts.yml"));
    }

    #[tokio::test]
    async fn backup_rejects_missing_config_dir() {
        let args = HaBackupArgs {
            config_path: "/nonexistent/homeassistant/config/path".to_string(),
            destination: "/tmp/homeassistant-test-dest".to_string(),
            dry_run: false,
        };
        let err = backup_config(&args).await.unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[tokio::test]
    async fn restore_rejects_missing_tarball() {
        let args = HaRestoreArgs {
            from: "/nonexistent/backup.tar.gz".to_string(),
            config_path: "/tmp/homeassistant-test-restore".to_string(),
            dry_run: false,
        };
        let err = restore_config(args).await.unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    fn fixture_config(root: &Path) -> std::path::PathBuf {
        let config = root.join("config");
        std::fs::create_dir_all(config.join(".storage")).unwrap();
        std::fs::write(config.join("configuration.yaml"), b"default_config:").unwrap();
        std::fs::write(config.join(".storage").join("core.config"), b"{}").unwrap();
        std::fs::create_dir_all(config.join("deps")).unwrap();
        std::fs::write(config.join("deps").join("junk"), b"x").unwrap();
        config
    }

    async fn executed_backup(root: &Path) -> HaBackupOutput {
        let config = fixture_config(root);
        backup_config(&HaBackupArgs {
            config_path: config.to_string_lossy().into_owned(),
            destination: root.join("backups").to_string_lossy().into_owned(),
            dry_run: false,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn backup_defaults_to_dry_run_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let config = fixture_config(tmp.path());
        let dest = tmp.path().join("backups");
        let out = backup_config(&HaBackupArgs {
            config_path: config.to_string_lossy().into_owned(),
            destination: dest.to_string_lossy().into_owned(),
            dry_run: true,
        })
        .await
        .unwrap();
        assert!(out.dry_run);
        assert!(out.sha256.is_none());
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn backup_writes_sha256sum_compatible_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        let sha = out.sha256.clone().unwrap();
        let sidecar = std::fs::read_to_string(&out.checksum_path).unwrap();
        let name = Path::new(&out.archive)
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert_eq!(sidecar, format!("{sha}  {name}\n"));
        assert_eq!(
            sha,
            plugin_toolkit::hash::sha256_file(Path::new(&out.archive)).unwrap()
        );
    }

    #[tokio::test]
    async fn restore_dry_run_verifies_but_does_not_extract() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        let target = tmp.path().join("restored");
        let r = restore_config(HaRestoreArgs {
            from: out.archive.clone(),
            config_path: target.to_string_lossy().into_owned(),
            dry_run: true,
        })
        .await
        .unwrap();
        assert!(r.dry_run && !r.restored);
        assert_eq!(Some(r.verified_sha256), out.sha256);
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn restore_refuses_tampered_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        let mut bytes = std::fs::read(&out.archive).unwrap();
        bytes.push(0);
        std::fs::write(&out.archive, bytes).unwrap();
        let target = tmp.path().join("restored");
        let err = restore_config(HaRestoreArgs {
            from: out.archive.clone(),
            config_path: target.to_string_lossy().into_owned(),
            dry_run: false,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn restore_refuses_archive_without_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        std::fs::remove_file(&out.checksum_path).unwrap();
        let err = restore_config(HaRestoreArgs {
            from: out.archive.clone(),
            config_path: tmp.path().join("r").to_string_lossy().into_owned(),
            dry_run: false,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[tokio::test]
    async fn restore_refuses_malformed_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        std::fs::write(&out.checksum_path, "nothex  x\n").unwrap();
        let err = verify_archive(&out.archive).err().unwrap();
        assert!(err.to_string().contains("malformed"), "{err}");
    }

    #[tokio::test]
    async fn restore_extracts_the_verified_copy_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        let (_, copy) = verify_archive(&out.archive).unwrap();
        let dir = copy.dir.clone();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // Swapping the original after verification does not affect the copy.
        std::fs::write(&out.archive, b"tampered").unwrap();
        assert_eq!(
            plugin_toolkit::hash::sha256_file(&copy.file).unwrap(),
            out.sha256.clone().unwrap()
        );
        drop(copy);
        assert!(!dir.exists());
    }

    #[test]
    fn sidecar_write_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("x.tar.gz");
        let c = tmp.path().join("x.tar.gz.sha256");
        write_sidecar(&a.to_string_lossy(), &c.to_string_lossy(), "ab").unwrap();
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "ab  x.tar.gz\n");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn backup_then_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let out = executed_backup(tmp.path()).await;
        assert!(Path::new(&out.archive).is_file());

        let restore_target = tmp.path().join("restored");
        let r = restore_config(HaRestoreArgs {
            from: out.archive.clone(),
            config_path: restore_target.to_string_lossy().into_owned(),
            dry_run: false,
        })
        .await
        .unwrap();
        assert!(r.restored && !r.dry_run);

        assert!(restore_target.join("configuration.yaml").is_file());
        assert!(restore_target
            .join(".storage")
            .join("core.config")
            .is_file());
        // deps was excluded from the archive
        assert!(!restore_target.join("deps").exists());
    }
}
