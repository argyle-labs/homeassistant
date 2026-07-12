//! Home Assistant tool surface.
//!
//! Endpoint registry: `home-assistant.{list, detail, create, update, delete}`
//! — generated wholesale by `#[endpoint_resource]`. Hand-written tools:
//!   - `home-assistant.entities`   list entities (optionally domain-filtered)
//!   - `home-assistant.entity`     single entity state
//!   - `home-assistant.automations`
//!   - `home-assistant.service`    invoke an HA service
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

fn make_client(name: &str) -> Result<Client> {
    let row = endpoint_db::get(name)?
        .with_context(|| format!("home assistant endpoint '{name}' not registered"))?;
    if !row.enabled {
        bail!("home assistant endpoint '{name}' is disabled");
    }
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
