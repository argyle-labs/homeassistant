//! Supervisor network mounts (`/mounts`): list, manage, and diagnose.
//!
//! HA reaches backup targets and media over Supervisor-managed SMB/NFS mounts.
//! The Supervisor's own "mount is down" conflates a stale session (server
//! fine; `reload` fixes it) with a dead server. [`classify`] separates them by
//! probing the server's real service ports.
//!
//! All calls go through the WebSocket `supervisor/api` passthrough, because a
//! long-lived token gets 401 on REST `/api/hassio/*`. The plugin controls the
//! Supervisor; it never mounts anything itself.
#![allow(clippy::disallowed_types)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Map, Value};

use crate::ws::{HaWs, WsTransport};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const SECRET_PROVIDER: &str = "home-assistant";

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct SvMount {
    pub name: String,
    /// `cifs` or `nfs`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `backup`, `media`, or `share`.
    pub usage: String,
    pub server: String,
    pub port: Option<u16>,
    /// NFS export path.
    pub path: Option<String>,
    /// CIFS share.
    pub share: Option<String>,
    pub read_only: bool,
    pub state: Option<String>,
    pub user_path: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Default,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct MountList {
    pub default_backup_mount: Option<String>,
    pub mounts: Vec<SvMount>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

pub fn parse_list(data: &Value) -> MountList {
    MountList {
        default_backup_mount: s(&data["default_backup_mount"]),
        mounts: data["mounts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                Some(SvMount {
                    name: s(&m["name"])?,
                    kind: s(&m["type"]).unwrap_or_default(),
                    usage: s(&m["usage"]).unwrap_or_default(),
                    server: s(&m["server"]).unwrap_or_default(),
                    port: m["port"].as_u64().and_then(|p| u16::try_from(p).ok()),
                    path: s(&m["path"]),
                    share: s(&m["share"]),
                    read_only: m["read_only"].as_bool().unwrap_or(false),
                    state: s(&m["state"]),
                    user_path: s(&m["user_path"]),
                })
            })
            .collect(),
    }
}

async fn supervisor<T: WsTransport>(
    ws: &mut HaWs<T>,
    method: &str,
    endpoint: &str,
    data: Option<Value>,
) -> Result<Value> {
    let mut f = json!({"endpoint": endpoint, "method": method});
    if let Some(d) = data {
        f["data"] = d;
    }
    ws.call("supervisor/api", f).await
}

pub(crate) async fn list<T: WsTransport>(ws: &mut HaWs<T>) -> Result<MountList> {
    Ok(parse_list(&supervisor(ws, "get", "/mounts", None).await?))
}

// ── health ────────────────────────────────────────────────────────────────

/// Ports that prove the storage service is up, by mount type.
pub fn service_ports(m: &SvMount) -> Vec<u16> {
    match (m.kind.as_str(), m.port) {
        (_, Some(p)) => vec![p],
        ("nfs", None) => vec![2049, 111],
        ("cifs", None) => vec![445],
        _ => vec![],
    }
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct MountHealth {
    pub name: String,
    pub state: Option<String>,
    /// `healthy`, `stale_session`, `server_down`, or `unknown`.
    pub condition: String,
    /// Ports probed and whether each answered.
    pub probes: Vec<(u16, bool)>,
    pub fix: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Default,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub mounts: Vec<MountHealth>,
    /// Fleet-level alerts: default backup mount missing, no active backup mount.
    pub alerts: Vec<String>,
}

/// The NFS primary port (2049) or the single SMB port decides reachability;
/// rpcbind (111) is informational for NFSv4.
pub fn classify(m: &SvMount, probes: &[(u16, bool)]) -> MountHealth {
    let reachable = match m.kind.as_str() {
        "nfs" => probes
            .iter()
            .find(|(p, _)| *p == m.port.unwrap_or(2049))
            .map(|(_, ok)| *ok),
        _ => probes.first().map(|(_, ok)| *ok),
    };
    let (condition, fix) = match (m.state.as_deref(), reachable) {
        (Some("active"), _) => ("healthy", None),
        (_, Some(true)) => (
            "stale_session",
            Some(format!("home-assistant.mount-reload --name {}", m.name)),
        ),
        (_, Some(false)) => (
            "server_down",
            Some(format!(
                "storage server {} is unreachable; fix the server",
                m.server
            )),
        ),
        (_, None) => ("unknown", None),
    };
    MountHealth {
        name: m.name.clone(),
        state: m.state.clone(),
        condition: condition.into(),
        probes: probes.to_vec(),
        fix,
    }
}

pub fn fleet_alerts(l: &MountList, health: &[MountHealth]) -> Vec<String> {
    let mut a = Vec::new();
    if let Some(d) = &l.default_backup_mount {
        if !l.mounts.iter().any(|m| &m.name == d) {
            a.push(format!(
                "default_backup_mount '{d}' does not exist; recreate it or reset the option"
            ));
        }
    }
    let backup: Vec<&SvMount> = l.mounts.iter().filter(|m| m.usage == "backup").collect();
    if !backup.is_empty()
        && !backup.iter().any(|m| {
            health
                .iter()
                .any(|h| h.name == m.name && h.condition == "healthy")
        })
    {
        a.push("no backup mount is active: backups to network storage will fail".into());
    }
    a
}

/// TCP connect off the runtime thread; `false` on refusal or timeout.
async fn probe(host: String, port: u16) -> bool {
    let done: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let w = Arc::clone(&done);
    std::thread::spawn(move || {
        use std::net::{TcpStream, ToSocketAddrs};
        let ok = (host.as_str(), port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .is_some_and(|addr| TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok());
        if let Ok(mut g) = w.lock() {
            *g = Some(ok);
        }
    });
    let poll = async move {
        loop {
            if let Some(v) = done.lock().ok().and_then(|mut g| g.take()) {
                return v;
            }
            plugin_toolkit::time::sleep(Duration::from_millis(25)).await;
        }
    };
    plugin_toolkit::time::timeout(PROBE_TIMEOUT * 2, poll)
        .await
        .unwrap_or(false)
}

// ── mutations ─────────────────────────────────────────────────────────────

/// A planned Supervisor call; credential values never appear in it.
#[derive(
    Debug,
    Clone,
    PartialEq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct PlannedCall {
    pub method: String,
    pub endpoint: String,
    /// Body with secrets shown as `<secret:NAME>`.
    pub body: Option<Value>,
}

#[derive(
    Debug,
    Clone,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct MountChangeOutput {
    pub dry_run: bool,
    pub call: PlannedCall,
    /// The mount before the change, when it existed.
    pub before: Option<SvMount>,
    /// The re-read result after an executed change.
    pub after: Option<MountList>,
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
    Clone,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct MountSpecArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    /// Mount name (letters, digits, underscore).
    #[arg(long)]
    pub name: String,
    /// `cifs` or `nfs`.
    #[arg(long = "type")]
    #[serde(rename = "type")]
    pub kind: String,
    /// `backup`, `media`, or `share`.
    #[arg(long)]
    pub usage: String,
    #[arg(long)]
    pub server: String,
    #[arg(long)]
    #[serde(default)]
    pub port: Option<u16>,
    /// NFS export path.
    #[arg(long)]
    #[serde(default)]
    pub path: Option<String>,
    /// CIFS share.
    #[arg(long)]
    #[serde(default)]
    pub share: Option<String>,
    /// CIFS protocol version (e.g. `3.1.1`).
    #[arg(long)]
    #[serde(default)]
    pub version: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub read_only: bool,
    /// Orca secret holding the CIFS username (default `home-assistant.<name>.username`).
    #[arg(long)]
    #[serde(default)]
    pub username_secret: Option<String>,
    /// Orca secret holding the CIFS password (default `home-assistant.<name>.password`).
    #[arg(long)]
    #[serde(default)]
    pub password_secret: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub execute: bool,
}

/// Secret names for a CIFS mount's credentials.
pub fn secret_names(a: &MountSpecArgs) -> (String, String) {
    (
        a.username_secret.clone().unwrap_or_else(|| {
            plugin_toolkit::secrets::scoped_name(SECRET_PROVIDER, &a.name, "username")
        }),
        a.password_secret.clone().unwrap_or_else(|| {
            plugin_toolkit::secrets::scoped_name(SECRET_PROVIDER, &a.name, "password")
        }),
    )
}

/// Validates the spec and builds the request body. `resolve` maps a secret
/// name to its value; the dry run passes a resolver that returns placeholders.
pub fn build_body(
    a: &MountSpecArgs,
    include_name: bool,
    resolve: &dyn Fn(&str) -> Result<String>,
) -> Result<Value> {
    if !valid_name(&a.name) {
        bail!(
            "mount name '{}' may only contain letters, digits, and underscores",
            a.name
        );
    }
    if !["backup", "media", "share"].contains(&a.usage.as_str()) {
        bail!("usage must be backup, media, or share");
    }
    if a.usage == "backup" && a.read_only {
        bail!("a backup mount cannot be read-only");
    }
    if a.server.trim().is_empty() {
        bail!("server is required");
    }
    let mut b = Map::new();
    if include_name {
        b.insert("name".into(), a.name.clone().into());
    }
    b.insert("type".into(), a.kind.clone().into());
    b.insert("usage".into(), a.usage.clone().into());
    b.insert("server".into(), a.server.clone().into());
    if let Some(p) = a.port {
        b.insert("port".into(), p.into());
    }
    if a.usage != "backup" {
        b.insert("read_only".into(), a.read_only.into());
    }
    match a.kind.as_str() {
        "nfs" => {
            let p = a
                .path
                .as_deref()
                .filter(|p| p.starts_with('/'))
                .context("nfs needs an absolute --path")?;
            b.insert("path".into(), p.into());
        }
        "cifs" => {
            let share = a
                .share
                .as_deref()
                .filter(|s| !s.is_empty())
                .context("cifs needs --share")?;
            b.insert("share".into(), share.into());
            if let Some(v) = &a.version {
                b.insert("version".into(), v.clone().into());
            }
            let (u, p) = secret_names(a);
            b.insert("username".into(), resolve(&u)?.into());
            b.insert("password".into(), resolve(&p)?.into());
        }
        other => bail!("type must be cifs or nfs, got '{other}'"),
    }
    Ok(Value::Object(b))
}

fn placeholder(name: &str) -> Result<String> {
    Ok(format!("<secret:{name}>"))
}

/// Placeholder for the plan, after confirming the secret exists, so a dry run
/// fails where the execute would.
fn checked_placeholder(name: &str, exists: &dyn Fn(&str) -> Result<bool>) -> Result<String> {
    if !exists(name)? {
        bail!("secret '{name}' does not exist; create it before the mount");
    }
    placeholder(name)
}

fn secret_exists(name: &str) -> Result<bool> {
    plugin_toolkit::secrets::exists(name)
}

/// Every field the request set must read back unchanged.
pub fn verify_fields(a: &MountSpecArgs, after: &MountList) -> Result<()> {
    let m = after
        .mounts
        .iter()
        .find(|m| m.name == a.name)
        .with_context(|| format!("mount '{}' is missing after the call", a.name))?;
    let mut diffs = Vec::new();
    let mut cmp = |field: &str, want: String, got: String| {
        if want != got {
            diffs.push(format!("{field}: wanted {want}, got {got}"));
        }
    };
    cmp("type", a.kind.clone(), m.kind.clone());
    cmp("usage", a.usage.clone(), m.usage.clone());
    cmp("server", a.server.clone(), m.server.clone());
    if let Some(p) = a.port {
        cmp(
            "port",
            p.to_string(),
            m.port.map_or("none".into(), |x| x.to_string()),
        );
    }
    if a.kind == "nfs" {
        cmp(
            "path",
            a.path.clone().unwrap_or_default(),
            m.path.clone().unwrap_or_default(),
        );
    } else {
        cmp(
            "share",
            a.share.clone().unwrap_or_default(),
            m.share.clone().unwrap_or_default(),
        );
    }
    if a.usage != "backup" {
        cmp(
            "read_only",
            a.read_only.to_string(),
            m.read_only.to_string(),
        );
    }
    if !diffs.is_empty() {
        bail!("mount '{}' not updated: {}", a.name, diffs.join("; "));
    }
    Ok(())
}

fn real_secret(name: &str) -> Result<String> {
    plugin_toolkit::secrets::get_required(name)
}

pub(crate) async fn change<T: WsTransport>(
    ws: &mut HaWs<T>,
    method: &str,
    endpoint: String,
    shown: Option<Value>,
    real: Option<Value>,
    name: &str,
    execute: bool,
) -> Result<MountChangeOutput> {
    let before = list(ws).await?.mounts.into_iter().find(|m| m.name == name);
    let call = PlannedCall {
        method: method.into(),
        endpoint: endpoint.clone(),
        body: shown,
    };
    if !execute {
        return Ok(MountChangeOutput {
            dry_run: true,
            call,
            before,
            after: None,
        });
    }
    supervisor(ws, method, &endpoint, real).await?;
    let after = list(ws).await?;
    Ok(MountChangeOutput {
        dry_run: false,
        call,
        before,
        after: Some(after),
    })
}

fn verify_present(out: &MountChangeOutput, name: &str, want: bool) -> Result<()> {
    let present = out
        .after
        .as_ref()
        .is_some_and(|a| a.mounts.iter().any(|m| m.name == name));
    if out.after.is_some() && present != want {
        bail!(
            "mount '{name}' {} after the call",
            if want { "is missing" } else { "still exists" }
        );
    }
    Ok(())
}

// ── tools ─────────────────────────────────────────────────────────────────

/// **List Supervisor mounts** (read-only) with state and the default backup mount.
#[endpoint_tool(domain = "home-assistant", verb = "mount-list", resolve = crate::tools::endpoint_config)]
async fn ha_mount_list(cfg: crate::Config) -> Result<MountList> {
    let mut ws = crate::ws::connect(&cfg).await?;
    list(&mut ws).await
}

/// **One Supervisor mount** (read-only).
#[endpoint_tool(domain = "home-assistant", verb = "mount-detail", resolve = crate::tools::endpoint_config)]
async fn ha_mount_detail(cfg: crate::Config, #[arg(long)] name: String) -> Result<SvMount> {
    let mut ws = crate::ws::connect(&cfg).await?;
    list(&mut ws)
        .await?
        .mounts
        .into_iter()
        .find(|m| m.name == name)
        .with_context(|| format!("no mount named '{name}'"))
}

/// **Diagnose Supervisor mounts** (read-only): probes each server's service
/// ports and tells a stale session (server up; reload) from a dead server,
/// plus default-backup-mount drift and "no active backup mount".
#[endpoint_tool(domain = "home-assistant", verb = "mount-health", resolve = crate::tools::endpoint_config)]
async fn ha_mount_health(cfg: crate::Config) -> Result<HealthReport> {
    let mut ws = crate::ws::connect(&cfg).await?;
    let l = list(&mut ws).await?;
    let mut mounts = Vec::new();
    for m in &l.mounts {
        let mut probes = Vec::new();
        for p in service_ports(m) {
            probes.push((p, probe(m.server.clone(), p).await));
        }
        mounts.push(classify(m, &probes));
    }
    let alerts = fleet_alerts(&l, &mounts);
    Ok(HealthReport { mounts, alerts })
}

/// **Create a Supervisor mount** [MUTATES STATE]. Requires admin, for dry
/// runs too. Dry-run unless `execute`. CIFS credentials come from orca
/// secrets, never arguments; the plan shows only their names.
#[orca_tool(domain = "home-assistant", verb = "mount-create", role = "admin")]
async fn ha_mount_create(args: MountSpecArgs, ctx: &ToolCtx) -> Result<MountChangeOutput> {
    crate::authz::require_admin(ctx)?;
    let shown = build_body(&args, true, &|n| checked_placeholder(n, &secret_exists))?;
    let real = if args.execute {
        Some(build_body(&args, true, &real_secret)?)
    } else {
        None
    };
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    let out = change(
        &mut ws,
        "post",
        "/mounts".into(),
        Some(shown),
        real,
        &args.name,
        args.execute,
    )
    .await?;
    verify_present(&out, &args.name, true)?;
    Ok(out)
}

/// **Update a Supervisor mount** [MUTATES STATE]. Requires admin, for dry
/// runs too. Dry-run unless `execute`.
#[orca_tool(domain = "home-assistant", verb = "mount-update", role = "admin")]
async fn ha_mount_update(args: MountSpecArgs, ctx: &ToolCtx) -> Result<MountChangeOutput> {
    crate::authz::require_admin(ctx)?;
    let shown = build_body(&args, false, &|n| checked_placeholder(n, &secret_exists))?;
    let real = if args.execute {
        Some(build_body(&args, false, &real_secret)?)
    } else {
        None
    };
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    let endpoint = format!("/mounts/{}", args.name);
    let out = change(
        &mut ws,
        "put",
        endpoint,
        Some(shown),
        real,
        &args.name,
        args.execute,
    )
    .await?;
    if let Some(after) = &out.after {
        verify_fields(&args, after)?;
    }
    Ok(out)
}

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct MountNameArgs {
    #[arg(long)]
    pub endpoint: String,
    #[arg(long)]
    pub name: String,
    /// Delete even the default backup mount.
    #[arg(long)]
    #[serde(default)]
    pub allow_default: bool,
    #[arg(long)]
    #[serde(default)]
    pub execute: bool,
}

/// **Delete a Supervisor mount** [MUTATES STATE]. Requires admin, for dry
/// runs too. Dry-run unless `execute`. Refuses the default backup mount
/// without `allowDefault`: deleting it orphans the backup estate.
#[orca_tool(domain = "home-assistant", verb = "mount-delete", role = "admin")]
async fn ha_mount_delete(args: MountNameArgs, ctx: &ToolCtx) -> Result<MountChangeOutput> {
    crate::authz::require_admin(ctx)?;
    if !valid_name(&args.name) {
        bail!("invalid mount name '{}'", args.name);
    }
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    let l = list(&mut ws).await?;
    if l.default_backup_mount.as_deref() == Some(args.name.as_str()) && !args.allow_default {
        bail!(
            "'{}' is the default backup mount; pass allowDefault to delete it",
            args.name
        );
    }
    let out = change(
        &mut ws,
        "delete",
        format!("/mounts/{}", args.name),
        None,
        None,
        &args.name,
        args.execute,
    )
    .await?;
    verify_present(&out, &args.name, false)?;
    Ok(out)
}

/// **Reload a Supervisor mount** [MUTATES STATE]: discards a stale session
/// and mounts fresh. Requires admin, for dry runs too. Dry-run unless `execute`.
#[orca_tool(domain = "home-assistant", verb = "mount-reload", role = "admin")]
async fn ha_mount_reload(args: MountNameArgs, ctx: &ToolCtx) -> Result<MountChangeOutput> {
    crate::authz::require_admin(ctx)?;
    if !valid_name(&args.name) {
        bail!("invalid mount name '{}'", args.name);
    }
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    change(
        &mut ws,
        "post",
        format!("/mounts/{}/reload", args.name),
        None,
        None,
        &args.name,
        args.execute,
    )
    .await
}

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct MountOptionsArgs {
    #[arg(long)]
    pub endpoint: String,
    /// Mount to use for backups; must exist with usage `backup`.
    #[arg(long)]
    pub default_backup_mount: String,
    #[arg(long)]
    #[serde(default)]
    pub execute: bool,
}

/// **Set the default backup mount** [MUTATES STATE]. Requires admin, for dry
/// runs too. Dry-run unless `execute`; verified by re-reading.
#[orca_tool(domain = "home-assistant", verb = "mount-options", role = "admin")]
async fn ha_mount_options(args: MountOptionsArgs, ctx: &ToolCtx) -> Result<MountChangeOutput> {
    crate::authz::require_admin(ctx)?;
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    let l = list(&mut ws).await?;
    if !l
        .mounts
        .iter()
        .any(|m| m.name == args.default_backup_mount && m.usage == "backup")
    {
        bail!(
            "'{}' is not an existing backup mount",
            args.default_backup_mount
        );
    }
    let body = json!({"default_backup_mount": args.default_backup_mount});
    let out = change(
        &mut ws,
        "post",
        "/mounts/options".into(),
        Some(body.clone()),
        Some(body),
        &args.default_backup_mount,
        args.execute,
    )
    .await?;
    if let Some(after) = &out.after {
        if after.default_backup_mount.as_deref() != Some(args.default_backup_mount.as_str()) {
            bail!("default_backup_mount was not applied");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::testing::{assert_refused, ctx};
    use crate::ws::testing::FakeHa;

    fn willow(state: &str) -> SvMount {
        SvMount {
            name: "willow".into(),
            kind: "nfs".into(),
            usage: "backup".into(),
            server: "10.0.0.10".into(),
            port: None,
            path: Some("/mnt/user/backups/home-assistant".into()),
            share: None,
            read_only: false,
            state: Some(state.into()),
            user_path: None,
        }
    }

    #[test]
    fn parses_supervisor_mounts() {
        let l = parse_list(&json!({"default_backup_mount": "willow", "mounts": [
            {"name": "willow", "type": "nfs", "usage": "backup", "server": "10.0.0.10",
             "path": "/mnt/user/backups/home-assistant", "state": "active", "read_only": false}
        ]}));
        assert_eq!(l.default_backup_mount.as_deref(), Some("willow"));
        assert_eq!(l.mounts[0], willow("active"));
    }

    #[test]
    fn classify_separates_stale_session_from_dead_server() {
        assert_eq!(classify(&willow("active"), &[]).condition, "healthy");
        let stale = classify(&willow("inactive"), &[(2049, true), (111, false)]);
        assert_eq!(stale.condition, "stale_session");
        assert!(stale.fix.unwrap().contains("mount-reload --name willow"));
        assert_eq!(
            classify(&willow("inactive"), &[(2049, false), (111, true)]).condition,
            "server_down"
        );
        assert_eq!(classify(&willow("failed"), &[]).condition, "unknown");
        assert_eq!(service_ports(&willow("x")), [2049, 111]);
    }

    #[test]
    fn subdirectory_of_an_export_is_not_flagged() {
        let l = MountList {
            default_backup_mount: Some("willow".into()),
            mounts: vec![willow("active")],
        };
        let h = vec![classify(&willow("active"), &[])];
        assert!(fleet_alerts(&l, &h).is_empty());
    }

    #[test]
    fn fleet_alerts_catch_drift_and_dead_backups() {
        let l = MountList {
            default_backup_mount: Some("gone".into()),
            mounts: vec![willow("inactive")],
        };
        let h = vec![classify(&willow("inactive"), &[(2049, true)])];
        let a = fleet_alerts(&l, &h);
        assert!(a.iter().any(|x| x.contains("'gone' does not exist")));
        assert!(a
            .iter()
            .any(|x| x.contains("backups to network storage will fail")));
    }

    fn cifs_args() -> MountSpecArgs {
        MountSpecArgs {
            endpoint: "ha".into(),
            name: "media_share".into(),
            kind: "cifs".into(),
            usage: "media".into(),
            server: "10.0.0.10".into(),
            port: None,
            path: None,
            share: Some("data".into()),
            version: Some("3.1.1".into()),
            read_only: true,
            username_secret: None,
            password_secret: None,
            execute: false,
        }
    }

    #[test]
    fn cifs_credentials_come_from_secrets_and_never_show() {
        let b = build_body(&cifs_args(), true, &placeholder).unwrap();
        assert_eq!(
            b["username"],
            "<secret:home-assistant.media_share.username>"
        );
        assert_eq!(
            b["password"],
            "<secret:home-assistant.media_share.password>"
        );
        let real = build_body(&cifs_args(), true, &|n| Ok(format!("value-of-{n}"))).unwrap();
        assert_eq!(
            real["password"],
            "value-of-home-assistant.media_share.password"
        );
    }

    #[test]
    fn body_validation() {
        let mut a = cifs_args();
        a.usage = "backup".into();
        assert!(build_body(&a, true, &placeholder)
            .unwrap_err()
            .to_string()
            .contains("read-only"));
        let mut a = cifs_args();
        a.name = "bad-name".into();
        assert!(build_body(&a, true, &placeholder).is_err());
        let mut a = cifs_args();
        a.kind = "nfs".into();
        assert!(build_body(&a, true, &placeholder)
            .unwrap_err()
            .to_string()
            .contains("--path"));
        let mut a = cifs_args();
        a.kind = "s3".into();
        assert!(build_body(&a, true, &placeholder).is_err());
    }

    #[tokio::test]
    async fn change_is_dry_run_by_default_and_sends_nothing() {
        let ha = FakeHa::new().on("supervisor/api", |v| {
            assert_eq!(v["method"], "get", "dry run must only read");
            json!({"mounts": [{"name": "willow", "type": "nfs", "usage": "backup", "server": "s", "state": "inactive"}]})
        });
        let seen = Arc::clone(&ha.seen);
        let mut ws = ha.session().await;
        let out = change(
            &mut ws,
            "post",
            "/mounts/willow/reload".into(),
            None,
            None,
            "willow",
            false,
        )
        .await
        .unwrap();
        assert!(out.dry_run && out.after.is_none());
        assert_eq!(out.before.unwrap().state.as_deref(), Some("inactive"));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn executed_change_re_reads() {
        let mut ws = FakeHa::new()
            .on("supervisor/api", |v| match v["method"].as_str().unwrap() {
                "get" => json!({"mounts": [{"name": "willow", "type": "nfs", "usage": "backup", "server": "s", "state": "active"}]}),
                _ => Value::Null,
            })
            .session()
            .await;
        let out = change(
            &mut ws,
            "post",
            "/mounts/willow/reload".into(),
            None,
            None,
            "willow",
            true,
        )
        .await
        .unwrap();
        assert!(!out.dry_run);
        verify_present(&out, "willow", true).unwrap();
        assert!(verify_present(&out, "willow", false).is_err());
    }

    #[test]
    fn dry_run_requires_the_secrets_to_exist() {
        let missing = |_: &str| Ok(false);
        let err =
            build_body(&cifs_args(), true, &|n| checked_placeholder(n, &missing)).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        let present = |_: &str| Ok(true);
        let b = build_body(&cifs_args(), true, &|n| checked_placeholder(n, &present)).unwrap();
        assert_eq!(
            b["password"],
            "<secret:home-assistant.media_share.password>"
        );
    }

    #[test]
    fn update_verification_catches_unapplied_fields() {
        let a = cifs_args();
        let applied = MountList {
            default_backup_mount: None,
            mounts: vec![SvMount {
                name: "media_share".into(),
                kind: "cifs".into(),
                usage: "media".into(),
                server: "10.0.0.10".into(),
                port: None,
                path: None,
                share: Some("data".into()),
                read_only: true,
                state: Some("active".into()),
                user_path: None,
            }],
        };
        verify_fields(&a, &applied).unwrap();
        let mut stale = applied.clone();
        stale.mounts[0].server = "10.0.0.11".into();
        stale.mounts[0].read_only = false;
        let err = verify_fields(&a, &stale).unwrap_err().to_string();
        assert!(err.contains("server") && err.contains("read_only"), "{err}");
        let gone = MountList::default();
        assert!(verify_fields(&a, &gone).is_err());
    }

    #[tokio::test]
    async fn every_mutating_verb_refuses_non_admin_before_planning() {
        for c in [ctx(None), ctx(Some("user"))] {
            assert_refused(&ha_mount_create(cifs_args(), &c).await.unwrap_err());
            assert_refused(&ha_mount_update(cifs_args(), &c).await.unwrap_err());
            let n = || MountNameArgs {
                endpoint: "nope".into(),
                name: "willow".into(),
                allow_default: false,
                execute: false,
            };
            assert_refused(&ha_mount_delete(n(), &c).await.unwrap_err());
            assert_refused(&ha_mount_reload(n(), &c).await.unwrap_err());
            let o = MountOptionsArgs {
                endpoint: "nope".into(),
                default_backup_mount: "willow".into(),
                execute: false,
            };
            assert_refused(&ha_mount_options(o, &c).await.unwrap_err());
        }
    }

    #[tokio::test]
    async fn probe_reports_closed_port() {
        // Port 9 on localhost is almost never open; either way it must return.
        let r = probe("127.0.0.1".into(), 9).await;
        assert!(!r);
    }
}
