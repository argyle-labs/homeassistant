//! Entity and area registry over the WebSocket API.
//!
//! API traps encoded here:
//! - `config/entity_registry/list` omits `aliases`; only the extended
//!   `get_entries` / `get` responses carry them, so [`entities`] reads those.
//! - `aliases: [null]` means *no aliases*; falsy values are dropped.
//! - A write can drop silently on a lost connection and still return a falsy
//!   result, so [`update_verified`] re-reads the entity and compares.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeSet;

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Map, Value};

use crate::ws::{HaWs, WsTransport};

const GET_ENTRIES_BATCH: usize = 100;

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntity {
    pub entity_id: String,
    /// User-set name; `None` means HA shows `original_name`.
    pub name: Option<String>,
    pub original_name: Option<String>,
    pub aliases: Vec<String>,
    pub area_id: Option<String>,
    pub device_id: Option<String>,
    pub hidden_by: Option<String>,
    pub disabled_by: Option<String>,
    pub platform: Option<String>,
}

impl RegistryEntity {
    /// The name HA displays and voice matches.
    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.original_name.as_deref())
    }
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Non-empty string aliases only; `[null]` and `""` mean none.
pub fn clean_aliases(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(opt_str)
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn from_entry(e: &Value) -> Option<RegistryEntity> {
    Some(RegistryEntity {
        entity_id: opt_str(&e["entity_id"])?,
        name: opt_str(&e["name"]),
        original_name: opt_str(&e["original_name"]),
        aliases: clean_aliases(&e["aliases"]),
        area_id: opt_str(&e["area_id"]),
        device_id: opt_str(&e["device_id"]),
        hidden_by: opt_str(&e["hidden_by"]),
        disabled_by: opt_str(&e["disabled_by"]),
        platform: opt_str(&e["platform"]),
    })
}

async fn get_one<T: WsTransport>(ws: &mut HaWs<T>, id: &str) -> Result<RegistryEntity> {
    let e = ws
        .call("config/entity_registry/get", json!({"entity_id": id}))
        .await?;
    from_entry(&e).with_context(|| format!("registry entry for '{id}' has no entity_id"))
}

/// Every registry entity with its aliases.
pub(crate) async fn entities<T: WsTransport>(ws: &mut HaWs<T>) -> Result<Vec<RegistryEntity>> {
    let listed = ws.call("config/entity_registry/list", Value::Null).await?;
    let ids: Vec<String> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| opt_str(&e["entity_id"]))
        .collect();
    let mut out = Vec::with_capacity(ids.len());
    let mut batched = true;
    for chunk in ids.chunks(GET_ENTRIES_BATCH) {
        if batched {
            match ws
                .call(
                    "config/entity_registry/get_entries",
                    json!({"entity_ids": chunk}),
                )
                .await
            {
                Ok(map) => {
                    for id in chunk {
                        match map.get(id).and_then(from_entry) {
                            Some(e) => out.push(e),
                            // A null entry means the entity left the registry mid-read.
                            None => continue,
                        }
                    }
                    continue;
                }
                Err(e) if e.to_string().contains("unknown_command") => batched = false,
                Err(e) => return Err(e),
            }
        }
        for id in chunk {
            out.push(get_one(ws, id).await?);
        }
    }
    out.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
    Ok(out)
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
pub struct Area {
    pub area_id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub floor_id: Option<String>,
}

pub(crate) async fn areas<T: WsTransport>(ws: &mut HaWs<T>) -> Result<Vec<Area>> {
    let v = ws.call("config/area_registry/list", Value::Null).await?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(Area {
                area_id: opt_str(&a["area_id"])?,
                name: opt_str(&a["name"]).unwrap_or_default(),
                aliases: clean_aliases(&a["aliases"]),
                floor_id: opt_str(&a["floor_id"]),
            })
        })
        .collect())
}

/// Fields `update_verified` may change.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct RegistryChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<String>,
}

/// Applies `change` and confirms it with a follow-up `get`; a write the
/// server did not keep is an error even when the update call "succeeded".
pub(crate) async fn update_verified<T: WsTransport>(
    ws: &mut HaWs<T>,
    entity_id: &str,
    change: &RegistryChange,
) -> Result<RegistryEntity> {
    let mut fields = Map::new();
    fields.insert("entity_id".into(), entity_id.into());
    // An empty name or area clears it: HA takes null to reset.
    let or_null = |v: &str| if v.is_empty() { Value::Null } else { v.into() };
    if let Some(n) = &change.name {
        fields.insert("name".into(), or_null(n));
    }
    if let Some(a) = &change.aliases {
        fields.insert("aliases".into(), json!(a));
    }
    if let Some(a) = &change.area_id {
        fields.insert("area_id".into(), or_null(a));
    }
    ws.call("config/entity_registry/update", Value::Object(fields))
        .await?;
    let after = get_one(ws, entity_id).await?;
    let want = |v: &str| (!v.is_empty()).then(|| v.to_string());
    if let Some(n) = &change.name {
        if after.name != want(n) {
            bail!("{entity_id}: name not applied (still {:?})", after.name);
        }
    }
    if let Some(a) = &change.aliases {
        let want: BTreeSet<&str> = a
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect();
        let got: BTreeSet<&str> = after.aliases.iter().map(String::as_str).collect();
        if want != got {
            bail!("{entity_id}: aliases not applied (have {got:?}, wanted {want:?})");
        }
    }
    if let Some(a) = &change.area_id {
        if after.area_id != want(a) {
            bail!("{entity_id}: area not applied (still {:?})", after.area_id);
        }
    }
    Ok(after)
}

/// **List the entity registry with aliases** (read-only): name, aliases,
/// area, device, hidden/disabled state, and platform for every entity.
#[endpoint_tool(domain = "home-assistant", verb = "registry-entities", resolve = crate::tools::endpoint_config)]
async fn ha_registry_entities(cfg: crate::Config) -> Result<Vec<RegistryEntity>> {
    let mut ws = crate::ws::connect(&cfg).await?;
    entities(&mut ws).await
}

/// **List areas** (read-only) with aliases and floor.
#[endpoint_tool(domain = "home-assistant", verb = "registry-areas", resolve = crate::tools::endpoint_config)]
async fn ha_registry_areas(cfg: crate::Config) -> Result<Vec<Area>> {
    let mut ws = crate::ws::connect(&cfg).await?;
    areas(&mut ws).await
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
pub struct RegistryUpdateArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    #[arg(long)]
    pub entity_id: String,
    #[arg(long)]
    #[serde(default)]
    pub name: Option<String>,
    /// Replaces the alias list; repeat for several.
    #[arg(long = "alias")]
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    #[arg(long)]
    #[serde(default)]
    pub area_id: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub execute: bool,
}

/// **Update one entity's name, aliases, or area** [MUTATES STATE]. Requires
/// admin, for dry runs too. Dry-run unless `execute`: the dry run returns the
/// current entry and the change. An executed write is confirmed by re-reading
/// the entity.
#[orca_tool(domain = "home-assistant", verb = "registry-update", role = "admin")]
async fn ha_registry_update(
    args: RegistryUpdateArgs,
    ctx: &ToolCtx,
) -> Result<RegistryUpdateOutput> {
    crate::authz::require_admin(ctx)?;
    let change = RegistryChange {
        name: args.name,
        aliases: args.aliases,
        area_id: args.area_id,
    };
    if change == RegistryChange::default() {
        bail!("nothing to change: pass --name, --alias, or --area-id");
    }
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    let before = get_one(&mut ws, &args.entity_id).await?;
    if !args.execute {
        return Ok(RegistryUpdateOutput {
            dry_run: true,
            before,
            change,
            after: None,
        });
    }
    let after = update_verified(&mut ws, &args.entity_id, &change).await?;
    Ok(RegistryUpdateOutput {
        dry_run: false,
        before,
        change,
        after: Some(after),
    })
}

#[derive(
    Debug,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct RegistryUpdateOutput {
    pub dry_run: bool,
    pub before: RegistryEntity,
    pub change: RegistryChange,
    /// The re-read entry after an executed write.
    pub after: Option<RegistryEntity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::testing::FakeHa;
    use std::sync::{Arc, Mutex};

    fn entry(id: &str, aliases: Value) -> Value {
        json!({"entity_id": id, "name": null, "original_name": "Lamp", "aliases": aliases,
               "area_id": "office", "hidden_by": null, "platform": "hue"})
    }

    #[test]
    fn aliases_drop_null_and_empty() {
        assert!(clean_aliases(&json!([null])).is_empty());
        assert_eq!(clean_aliases(&json!(["b", "", null, "a", "a"])), ["a", "b"]);
        assert!(clean_aliases(&Value::Null).is_empty());
    }

    #[tokio::test]
    async fn entities_read_aliases_from_get_entries_not_list() {
        let mut ws = FakeHa::new()
            // `list` deliberately carries no aliases.
            .on("config/entity_registry/list", |_| {
                json!([{"entity_id": "light.a"}, {"entity_id": "light.b"}, {"entity_id": "light.gone"}])
            })
            .on("config/entity_registry/get_entries", |_| {
                json!({"light.a": entry("light.a", json!([null])),
                       "light.b": entry("light.b", json!(["desk lamp"])),
                       "light.gone": null})
            })
            .session()
            .await;
        let es = entities(&mut ws).await.unwrap();
        assert_eq!(es.len(), 2);
        assert!(es[0].aliases.is_empty());
        assert_eq!(es[1].aliases, ["desk lamp"]);
        assert_eq!(es[1].display_name(), Some("Lamp"));
        assert_eq!(es[1].area_id.as_deref(), Some("office"));
    }

    #[tokio::test]
    async fn entities_fall_back_to_get_on_older_ha() {
        let mut ws = FakeHa::new()
            .on(
                "config/entity_registry/list",
                |_| json!([{"entity_id": "light.a"}]),
            )
            .on("config/entity_registry/get", |v| {
                entry(v["entity_id"].as_str().unwrap(), json!(["x"]))
            })
            .session()
            .await;
        let es = entities(&mut ws).await.unwrap();
        assert_eq!(es[0].aliases, ["x"]);
    }

    #[tokio::test]
    async fn update_is_verified_by_a_follow_up_get() {
        let stored = Arc::new(Mutex::new(json!([])));
        let w = Arc::clone(&stored);
        let r = Arc::clone(&stored);
        let mut ws = FakeHa::new()
            .on("config/entity_registry/update", move |v| {
                *w.lock().unwrap() = v["aliases"].clone();
                Value::Null
            })
            .on("config/entity_registry/get", move |_| {
                entry("light.a", r.lock().unwrap().clone())
            })
            .session()
            .await;
        let change = RegistryChange {
            aliases: Some(vec!["lamp".into()]),
            ..Default::default()
        };
        let after = update_verified(&mut ws, "light.a", &change).await.unwrap();
        assert_eq!(after.aliases, ["lamp"]);
    }

    #[tokio::test]
    async fn empty_name_and_area_clear_them() {
        let stored = Arc::new(Mutex::new(Value::Null));
        let (w, r) = (Arc::clone(&stored), Arc::clone(&stored));
        let mut ws = FakeHa::new()
            .on("config/entity_registry/update", move |v| {
                *w.lock().unwrap() = v.clone();
                Value::Null
            })
            .on("config/entity_registry/get", move |_| {
                let u = r.lock().unwrap().clone();
                json!({"entity_id": "light.a", "name": u["name"], "area_id": u["area_id"]})
            })
            .session()
            .await;
        let change = RegistryChange {
            name: Some(String::new()),
            area_id: Some(String::new()),
            ..Default::default()
        };
        let after = update_verified(&mut ws, "light.a", &change).await.unwrap();
        assert_eq!((after.name, after.area_id), (None, None));
        let sent = stored.lock().unwrap().clone();
        assert!(
            sent["name"].is_null() && sent["area_id"].is_null(),
            "{sent}"
        );
    }

    #[tokio::test]
    async fn silently_dropped_write_is_an_error() {
        let mut ws = FakeHa::new()
            .on("config/entity_registry/update", |_| Value::Bool(false))
            .on("config/entity_registry/get", |_| {
                entry("light.a", json!([null]))
            })
            .session()
            .await;
        let change = RegistryChange {
            aliases: Some(vec!["lamp".into()]),
            ..Default::default()
        };
        let err = update_verified(&mut ws, "light.a", &change)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not applied"), "{err}");
    }

    #[tokio::test]
    async fn registry_update_refuses_non_admin_before_planning() {
        use crate::authz::testing::{assert_refused, ctx};
        for c in [ctx(None), ctx(Some("user"))] {
            let args = RegistryUpdateArgs {
                // Unregistered: a refusal must come before the endpoint lookup.
                endpoint: "nope".into(),
                entity_id: "light.a".into(),
                name: Some("Lamp".into()),
                aliases: None,
                area_id: None,
                execute: false,
            };
            assert_refused(&ha_registry_update(args, &c).await.unwrap_err());
        }
    }

    #[tokio::test]
    async fn areas_parse() {
        let mut ws = FakeHa::new()
            .on("config/area_registry/list", |_| {
                json!([{"area_id": "office", "name": "Office", "aliases": [null], "floor_id": "f1"}])
            })
            .session()
            .await;
        let a = areas(&mut ws).await.unwrap();
        assert_eq!(a[0].name, "Office");
        assert!(a[0].aliases.is_empty());
    }
}
