//! Exposure of entities to voice assistants (`conversation`,
//! `cloud.google_assistant`, `cloud.alexa`): audit and ghost purge.
//!
//! Exposure is stored per entity in the entity registry options and listed
//! by `homeassistant/expose_entity/list`. Records outlive their entities: 95
//! ghost records were advertised to Google for entities that no longer
//! existed.
#![allow(clippy::disallowed_types)]

use std::collections::{BTreeMap, BTreeSet};

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Value};

use crate::ws::{HaWs, WsTransport};

/// Domains a voice command can actuate; only these should be reachable
/// identically everywhere.
const CONTROLLABLE: &[&str] = &[
    "climate",
    "cover",
    "fan",
    "input_boolean",
    "light",
    "lock",
    "media_player",
    "scene",
    "script",
    "switch",
    "vacuum",
    "valve",
];

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
pub struct ExposeAudit {
    /// Assistants seen in any record.
    pub assistants: Vec<String>,
    /// Per assistant, how many live entities it exposes.
    pub exposed_counts: BTreeMap<String, usize>,
    /// Records (exposed to at least one assistant) whose entity no longer exists.
    pub ghosts: Vec<String>,
    /// Live controllable entities exposed to no assistant.
    pub exposed_nowhere: Vec<String>,
    /// Live entities exposed to some assistants but not all: entity → missing assistants.
    pub inconsistent: BTreeMap<String, Vec<String>>,
}

/// `entity_id → assistant → exposed` from an `expose_entity/list` result.
pub fn records(list: &Value) -> BTreeMap<String, BTreeMap<String, bool>> {
    list["exposed_entities"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, m)| {
            let per: BTreeMap<String, bool> = m
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(a, v)| Some((a.clone(), v.as_bool()?)))
                .collect();
            (id.clone(), per)
        })
        .collect()
}

pub fn audit(list: &Value, live: &BTreeSet<String>) -> ExposeAudit {
    let recs = records(list);
    let assistants: BTreeSet<String> = recs.values().flat_map(|m| m.keys().cloned()).collect();
    let mut a = ExposeAudit {
        assistants: assistants.iter().cloned().collect(),
        ..Default::default()
    };
    for (id, per) in &recs {
        let on: BTreeSet<&String> = per.iter().filter(|(_, v)| **v).map(|(k, _)| k).collect();
        if !live.contains(id) {
            if !on.is_empty() {
                a.ghosts.push(id.clone());
            }
            continue;
        }
        for asst in &on {
            *a.exposed_counts.entry((*asst).clone()).or_default() += 1;
        }
        if !on.is_empty() && on.len() < assistants.len() {
            let missing = assistants
                .iter()
                .filter(|x| !on.contains(x))
                .cloned()
                .collect();
            a.inconsistent.insert(id.clone(), missing);
        }
    }
    a.exposed_nowhere = live
        .iter()
        .filter(|id| {
            let domain = id.split_once('.').map_or("", |(d, _)| d);
            CONTROLLABLE.contains(&domain)
                && recs.get(*id).is_none_or(|per| !per.values().any(|v| *v))
        })
        .cloned()
        .collect();
    a
}

async fn list_and_live<T: WsTransport>(ws: &mut HaWs<T>) -> Result<(Value, BTreeSet<String>)> {
    let list = ws
        .call("homeassistant/expose_entity/list", Value::Null)
        .await?;
    // An entity is live if it has a state or a registry entry: an unavailable
    // integration drops states but keeps its registry entries.
    let states = ws.call("get_states", Value::Null).await?;
    let registry = ws.call("config/entity_registry/list", Value::Null).await?;
    let live = [states, registry]
        .iter()
        .flat_map(|v| v.as_array().into_iter().flatten())
        .filter_map(|s| s["entity_id"].as_str().map(str::to_string))
        .collect();
    Ok((list, live))
}

/// HA's core state (`RUNNING`, `STARTING`, …) from `get_config`.
async fn core_state<T: WsTransport>(ws: &mut HaWs<T>) -> Result<String> {
    let cfg = ws.call("get_config", Value::Null).await?;
    Ok(cfg["state"].as_str().unwrap_or("unknown").to_string())
}

/// **Audit voice-assistant exposure** (read-only): ghost records, live
/// controllable entities exposed nowhere, and entities exposed to some
/// assistants but not others.
#[endpoint_tool(domain = "home-assistant", verb = "expose-audit", resolve = crate::tools::endpoint_config)]
async fn ha_expose_audit(cfg: crate::Config) -> Result<ExposeAudit> {
    let mut ws = crate::ws::connect(&cfg).await?;
    let (list, live) = list_and_live(&mut ws).await?;
    Ok(audit(&list, &live))
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
pub struct PurgeOutput {
    pub dry_run: bool,
    /// Ghost records un-exposed (or that would be).
    pub ghosts: Vec<String>,
    /// Ghosts still exposed after an executed purge.
    pub still_exposed: Vec<String>,
}

/// Un-exposes every ghost from every assistant, then re-reads to confirm.
pub(crate) async fn purge<T: WsTransport>(ws: &mut HaWs<T>, execute: bool) -> Result<PurgeOutput> {
    // While starting, integrations have not registered their entities yet, so
    // every exposed entity would look like a ghost.
    let state = core_state(ws).await?;
    if state != "RUNNING" {
        bail!("Home Assistant is {state}, not RUNNING; refusing to judge ghosts");
    }
    let (list, live) = list_and_live(ws).await?;
    let a = audit(&list, &live);
    if !execute || a.ghosts.is_empty() {
        return Ok(PurgeOutput {
            dry_run: !execute,
            ghosts: a.ghosts,
            still_exposed: Vec::new(),
        });
    }
    ws.call(
        "homeassistant/expose_entity",
        json!({"assistants": a.assistants, "entity_ids": a.ghosts, "should_expose": false}),
    )
    .await?;
    let (after, live_after) = list_and_live(ws).await?;
    let still = audit(&after, &live_after).ghosts;
    let still_exposed: Vec<String> = a
        .ghosts
        .iter()
        .filter(|g| still.contains(g))
        .cloned()
        .collect();
    if !still_exposed.is_empty() {
        bail!("purge not applied for: {}", still_exposed.join(", "));
    }
    Ok(PurgeOutput {
        dry_run: false,
        ghosts: a.ghosts,
        still_exposed,
    })
}

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct PurgeArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    #[arg(long)]
    /// Report the change without writing it.
    #[serde(default)]
    pub dry_run: bool,
}

/// **Un-expose ghost records** [MUTATES STATE]: records exposed to an
/// assistant for entities that no longer exist. Requires admin, for dry runs
/// too. Executes unless `dry_run`; an executed purge is confirmed by re-listing.
#[orca_tool(domain = "home-assistant", verb = "expose-purge", role = "admin")]
async fn ha_expose_purge(args: PurgeArgs, ctx: &ToolCtx) -> Result<PurgeOutput> {
    crate::authz::require_admin(ctx)?;
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    purge(&mut ws, !args.dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::testing::FakeHa;
    use std::sync::{Arc, Mutex};

    fn list() -> Value {
        json!({"exposed_entities": {
            "light.a": {"conversation": true, "cloud.google_assistant": true},
            "light.b": {"conversation": true, "cloud.google_assistant": false},
            "light.gone": {"conversation": false, "cloud.google_assistant": true},
            "switch.gone_off": {"conversation": false},
            "sensor.t": {"conversation": true}
        }})
    }

    fn live() -> BTreeSet<String> {
        ["light.a", "light.b", "sensor.t", "light.c", "sensor.x"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn audit_finds_ghosts_gaps_and_mismatches() {
        let a = audit(&list(), &live());
        assert_eq!(a.assistants, ["cloud.google_assistant", "conversation"]);
        assert_eq!(
            a.ghosts,
            ["light.gone"],
            "unexposed dead records are not ghosts"
        );
        assert_eq!(
            a.exposed_nowhere,
            ["light.c"],
            "sensors are not controllable"
        );
        assert_eq!(a.inconsistent["light.b"], ["cloud.google_assistant"]);
        assert_eq!(a.inconsistent["sensor.t"], ["cloud.google_assistant"]);
        assert!(!a.inconsistent.contains_key("light.a"));
        assert_eq!(a.exposed_counts["conversation"], 3);
    }

    fn fake(state: Arc<Mutex<Value>>, honor: bool) -> FakeHa {
        fake_in(state, honor, "RUNNING")
    }

    fn fake_in(state: Arc<Mutex<Value>>, honor: bool, core: &'static str) -> FakeHa {
        let r = Arc::clone(&state);
        FakeHa::new()
            .on("get_config", move |_| json!({"state": core}))
            .on(
                "config/entity_registry/list",
                |_| json!([{"entity_id": "light.offline_but_registered"}]),
            )
            .on("homeassistant/expose_entity/list", move |_| {
                r.lock().unwrap().clone()
            })
            .on("get_states", |_| {
                json!(["light.a", "light.b", "sensor.t", "light.c"]
                    .iter()
                    .map(|e| json!({"entity_id": e}))
                    .collect::<Vec<_>>())
            })
            .on("homeassistant/expose_entity", move |v| {
                if honor {
                    let mut s = state.lock().unwrap();
                    for id in v["entity_ids"].as_array().unwrap() {
                        for a in v["assistants"].as_array().unwrap() {
                            s["exposed_entities"][id.as_str().unwrap()][a.as_str().unwrap()] =
                                json!(false);
                        }
                    }
                }
                Value::Null
            })
    }

    #[tokio::test]
    async fn purge_refuses_non_admin_before_planning() {
        use crate::authz::testing::{assert_refused, ctx};
        for c in [ctx(None), ctx(Some("user"))] {
            let args = PurgeArgs {
                endpoint: "nope".into(),
                dry_run: true,
            };
            assert_refused(&ha_expose_purge(args, &c).await.unwrap_err());
        }
    }

    #[tokio::test]
    async fn purge_refuses_while_ha_is_not_running() {
        let state = Arc::new(Mutex::new(list()));
        let mut ws = fake_in(state, true, "STARTING").session().await;
        let err = purge(&mut ws, false).await.unwrap_err();
        assert!(err.to_string().contains("STARTING"), "{err}");
    }

    #[tokio::test]
    async fn registered_entity_without_state_is_not_a_ghost() {
        let mut l = list();
        l["exposed_entities"]["light.offline_but_registered"] = json!({"conversation": true});
        let state = Arc::new(Mutex::new(l));
        let mut ws = fake(state, true).session().await;
        let out = purge(&mut ws, false).await.unwrap();
        assert_eq!(out.ghosts, ["light.gone"]);
    }

    #[tokio::test]
    async fn purge_is_dry_run_by_default() {
        let state = Arc::new(Mutex::new(list()));
        let ha = fake(Arc::clone(&state), true);
        let seen = Arc::clone(&ha.seen);
        let mut ws = ha.session().await;
        let out = purge(&mut ws, false).await.unwrap();
        assert!(out.dry_run);
        assert_eq!(out.ghosts, ["light.gone"]);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|m| m["type"] != "homeassistant/expose_entity"));
    }

    #[tokio::test]
    async fn purge_unexposes_ghosts_and_verifies() {
        let state = Arc::new(Mutex::new(list()));
        let mut ws = fake(Arc::clone(&state), true).session().await;
        let out = purge(&mut ws, true).await.unwrap();
        assert!(!out.dry_run && out.still_exposed.is_empty());
        assert_eq!(
            state.lock().unwrap()["exposed_entities"]["light.gone"]["cloud.google_assistant"],
            false
        );
        assert_eq!(
            state.lock().unwrap()["exposed_entities"]["light.a"]["conversation"],
            true
        );
    }

    #[tokio::test]
    async fn purge_that_does_not_stick_is_an_error() {
        let state = Arc::new(Mutex::new(list()));
        let mut ws = fake(state, false).session().await;
        let err = purge(&mut ws, true).await.unwrap_err();
        assert!(err.to_string().contains("not applied"), "{err}");
    }
}
