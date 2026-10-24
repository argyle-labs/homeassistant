//! Home Assistant tool surface.
//!
//! Endpoint registry: `home-assistant.{list, detail, create, update, delete}`
//! — generated wholesale by `#[endpoint_resource]`. Hand-written tools:
//!   - `home-assistant.entities`   list entities (optionally domain-filtered)
//!   - `home-assistant.entity`     single entity state
//!   - `home-assistant.automations`
//!   - `home-assistant.service`    invoke an HA service
//!   - `home-assistant.voice-audit` (in [`crate::voice`]) read-only naming audit
//!
//! The endpoint tools use `#[endpoint_tool]`: the client-resolve + args-struct
//! + `#[orca_tool]` scaffolding is generated, so each tool is just its call.
//!
//! Imports flow through `plugin_toolkit::prelude::*` only.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json as sj;

use crate::{Client, Config, ServiceCall};

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.{list,detail,create,update,delete} — endpoint registry CRUD.
// ═══════════════════════════════════════════════════════════════════════════

#[endpoint_resource(plugin = "home-assistant", table = "homeassistant_endpoints")]
pub struct HaEndpoint {
    pub name: String,
    pub base_url: String,
    #[secret]
    pub token: String,
    pub enabled: bool,
}

// ── HTTP client helper ─────────────────────────────────────────────────────

pub(crate) fn make_client(name: &str) -> Result<Client> {
    let row = endpoint_db::get(name)?
        .with_context(|| format!("home assistant endpoint '{name}' not registered"))?;
    if !row.enabled {
        bail!("home assistant endpoint '{name}' is disabled");
    }
    crate::check_base_url(&row.base_url).map_err(|e| anyhow!(e))?;
    Ok(Client::new(Config::new(row.base_url, row.token)))
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.entities — list entities (optionally domain-filtered)
// ═══════════════════════════════════════════════════════════════════════════

#[endpoint_tool(domain = "home-assistant", verb = "entities")]
/// List entities, optionally filtered to one HA domain.
async fn ha_entities(
    client: Client,
    /// Optional HA domain filter (light, sensor, switch, …).
    #[arg(long)]
    domain: Option<String>,
) -> Result<JsonAny> {
    Ok(client.entity_list(domain.as_deref()).await?.into())
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.entity — single entity state
// ═══════════════════════════════════════════════════════════════════════════

#[endpoint_tool(domain = "home-assistant", verb = "entity")]
/// Fetch a single entity's current state.
async fn ha_entity(
    client: Client,
    /// Entity ID (e.g. "light.living_room").
    #[arg(long)]
    entity_id: String,
) -> Result<JsonAny> {
    Ok(client.entity_state(&entity_id).await?.into())
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.automations — list automations
// ═══════════════════════════════════════════════════════════════════════════

#[endpoint_tool(domain = "home-assistant", verb = "automations")]
/// List configured automations.
async fn ha_automations(client: Client) -> Result<JsonAny> {
    Ok(client.automation_list().await?.into())
}

// ═══════════════════════════════════════════════════════════════════════════
// home-assistant.service — invoke an HA service
// ═══════════════════════════════════════════════════════════════════════════

/// [MUTATES STATE] Invoke a Home Assistant service.
#[endpoint_tool(domain = "home-assistant", verb = "service", role = "admin")]
async fn ha_service(
    client: Client,
    /// HA service domain (light, switch, automation, …).
    #[arg(long)]
    service_domain: String,
    /// HA service name (turn_on, toggle, …).
    #[arg(long)]
    service_name: String,
    #[arg(long)] entity_id: Option<String>,
    /// Opaque free-form service-data — upstream-defined.
    #[arg(skip)]
    service_data: Option<sj::Map<String, sj::Value>>,
) -> Result<JsonAny> {
    let call = ServiceCall {
        domain: service_domain,
        service: service_name,
        entity_id,
        data: service_data.unwrap_or_default(),
    };
    Ok(client.service_call(&call).await?.into())
}

#[orca_struct(args)]
pub struct HaBackupsArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
}

/// List Home Assistant's native backups with each copy's agent, size, and protection.
#[orca_tool(domain = "home-assistant", verb = "backups", role = "admin")]
async fn ha_backups(args: HaBackupsArgs, ctx: &ToolCtx) -> Result<crate::backups::HaBackupList> {
    crate::authz::require_admin(ctx)?;
    make_client(&args.endpoint)?
        .backup_list(&args.endpoint)
        .await
}

#[orca_struct(args)]
pub struct HaBackupCheckArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    /// Hours after which the newest backup counts as stale.
    #[arg(long, default_value_t = 48)]
    #[serde(default = "default_max_age_hours")]
    pub max_age_hours: i64,
}

fn default_max_age_hours() -> i64 {
    48
}

/// Grade Home Assistant's native backups for freshness and off-box coverage.
/// Integrity is not verified: Home Assistant exposes no backup checksum.
/// Returns agent ids (operational mount names, scrubbed) but no backup names
/// or error text, so read-role callers see no free-form upstream content.
#[orca_tool(domain = "home-assistant", verb = "backup-check")]
async fn ha_backup_check(
    args: HaBackupCheckArgs,
    _ctx: &ToolCtx,
) -> Result<crate::backups::HaBackupCheck> {
    crate::backups::validate_max_age_hours(args.max_age_hours)?;
    make_client(&args.endpoint)?
        .backup_check(&args.endpoint, args.max_age_hours)
        .await
}

#[orca_struct(args)]
pub struct HaEndpointArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
}

/// List Home Assistant backup agents and locations, the last automatic backup result, and failed uploads.
#[orca_tool(domain = "home-assistant", verb = "backup-status", role = "read")]
async fn ha_backup_status(
    args: HaEndpointArgs,
    _ctx: &ToolCtx,
) -> Result<crate::backups::HaBackupStatus> {
    make_client(&args.endpoint)?
        .backup_status(&args.endpoint)
        .await
}

/// List Supervisor network storage mounts with usage, state, and failure issues. Never returns passwords.
#[orca_tool(domain = "home-assistant", verb = "mount-list", role = "read")]
async fn ha_mount_list(
    args: HaEndpointArgs,
    _ctx: &ToolCtx,
) -> Result<crate::supervisor::HaMountList> {
    make_client(&args.endpoint)?.mount_list().await
}

/// List open Home Assistant repair issues.
#[orca_tool(domain = "home-assistant", verb = "repairs-list", role = "read")]
async fn ha_repairs_list(
    args: HaEndpointArgs,
    _ctx: &ToolCtx,
) -> Result<crate::supervisor::HaRepairList> {
    make_client(&args.endpoint)?.repairs_list().await
}

#[orca_struct(args)]
pub struct HaMountRepairArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    /// Supervisor mount name.
    #[arg(long)]
    pub mount: String,
    /// New server host.
    #[arg(long)]
    #[serde(default)]
    pub server: Option<String>,
    /// New share (cifs) or export path (nfs).
    #[arg(long)]
    #[serde(default)]
    pub share: Option<String>,
    /// New cifs username.
    #[arg(long)]
    #[serde(default)]
    pub username: Option<String>,
    /// orca secret ref holding the cifs password; required for any cifs update.
    #[arg(long)]
    #[serde(default)]
    pub password_secret: Option<String>,
    /// Apply the change. Without it the tool only returns the plan.
    #[arg(long)]
    #[serde(default)]
    pub execute: bool,
}

/// [SENSITIVE] [MUTATES STATE] Reload a Supervisor mount, optionally updating server, share, or credentials first.
/// Returns the plan only unless `execute` is set.
#[orca_tool(domain = "home-assistant", verb = "mount-repair", role = "admin")]
async fn ha_mount_repair(
    args: HaMountRepairArgs,
    ctx: &ToolCtx,
) -> Result<crate::supervisor::HaMountRepair> {
    crate::authz::require_admin(ctx)?;
    let change = crate::supervisor::MountChange {
        server: args.server,
        share: args.share,
        username: args.username,
        password_secret: args.password_secret,
    };
    make_client(&args.endpoint)?
        .mount_repair(&args.mount, &change, args.execute, |name| {
            plugin_toolkit::secrets::get_required(name)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::testing::{assert_refused, ctx};

    #[tokio::test]
    async fn backups_list_requires_admin() {
        for role in [Some("user"), Some("viewer"), None] {
            let args = HaBackupsArgs {
                endpoint: "ha".into(),
            };
            assert_refused(&ha_backups(args, &ctx(role)).await.unwrap_err());
        }
        let args = HaBackupsArgs {
            endpoint: "ha".into(),
        };
        let err = ha_backups(args, &ctx(Some("admin"))).await.unwrap_err();
        assert!(!err.to_string().contains("admin role required"), "{err}");
    }

    #[tokio::test]
    async fn mount_repair_requires_admin() {
        let args = || HaMountRepairArgs {
            endpoint: "ha".into(),
            mount: "maple_backups".into(),
            server: None,
            share: None,
            username: None,
            password_secret: None,
            execute: false,
        };
        for role in [Some("user"), Some("viewer"), None] {
            assert_refused(&ha_mount_repair(args(), &ctx(role)).await.unwrap_err());
        }
        let err = ha_mount_repair(args(), &ctx(Some("admin")))
            .await
            .unwrap_err();
        assert!(!err.to_string().contains("admin role required"), "{err}");
    }

    #[tokio::test]
    async fn backup_check_is_not_admin_gated_and_validates_max_age() {
        let args = HaBackupCheckArgs {
            endpoint: "ha".into(),
            max_age_hours: 48,
        };
        let err = ha_backup_check(args, &ctx(Some("user"))).await.unwrap_err();
        assert!(!err.to_string().contains("admin role required"), "{err}");
        let args = HaBackupCheckArgs {
            endpoint: "ha".into(),
            max_age_hours: 0,
        };
        let err = ha_backup_check(args, &ctx(Some("user"))).await.unwrap_err();
        assert!(err.to_string().contains("max_age_hours"), "{err}");
    }
}
