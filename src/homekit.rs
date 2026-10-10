//! HomeKit bridge audit.
//!
//! Each HomeKit bridge or accessory is a config entry whose include filter
//! names entities. A filter naming a deleted entity makes the entry fail on
//! every startup. The filter is read from the entry's diagnostics
//! (`/api/diagnostics/config_entry/<id>`), which the REST API serves to a
//! long-lived token.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeSet;

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::Value;

use crate::Client;

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
pub struct BridgeStatus {
    pub entry_id: String,
    pub title: String,
    /// Config-entry state (`loaded`, `setup_error`, `setup_retry`, …).
    pub state: String,
    pub reason: Option<String>,
    /// `bridge` or `accessory`, when the diagnostics report it.
    pub mode: Option<String>,
    pub include_entities: Vec<String>,
    pub include_domains: Vec<String>,
    /// Included entities that do not exist.
    pub dead_refs: Vec<String>,
    /// The diagnostics carried no filter, so references were not checked.
    pub filter_unknown: bool,
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
pub struct BridgeAudit {
    pub bridges: Vec<BridgeStatus>,
    /// Entries not `loaded`.
    pub failing: Vec<String>,
    /// Exposed to Assist but in no HomeKit filter; `None` when Assist
    /// exposure could not be read.
    pub missing_from_homekit: Option<Vec<String>>,
    pub notes: Vec<String>,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().filter(|s| !s.is_empty()).map(str::to_string))
        .collect()
}

/// The entry options inside a diagnostics payload; their location has moved
/// between HA releases.
fn options_of(diag: &Value) -> Option<&Value> {
    let d = &diag["data"];
    [
        &d["options"],
        &d["config-entry"]["options"],
        &d["config_entry"]["options"],
        &d["entry"]["options"],
    ]
    .into_iter()
    .find(|o| o.is_object())
}

pub fn bridge_status(entry: &Value, diag: Option<&Value>, live: &BTreeSet<String>) -> BridgeStatus {
    let opts = diag.and_then(options_of);
    let filter = opts.map(|o| &o["filter"]);
    let include_entities = filter
        .map(|f| strings(&f["include_entities"]))
        .unwrap_or_default();
    let dead_refs = include_entities
        .iter()
        .filter(|e| !live.contains(*e))
        .cloned()
        .collect();
    BridgeStatus {
        entry_id: entry["entry_id"].as_str().unwrap_or_default().into(),
        title: entry["title"].as_str().unwrap_or_default().into(),
        state: entry["state"].as_str().unwrap_or("unknown").into(),
        reason: entry["reason"].as_str().map(str::to_string),
        mode: opts.and_then(|o| o["mode"].as_str()).map(str::to_string),
        include_domains: filter
            .map(|f| strings(&f["include_domains"]))
            .unwrap_or_default(),
        include_entities,
        dead_refs,
        filter_unknown: filter.is_none_or(|f| !f.is_object()),
    }
}

/// Assist-exposed entities that no bridge includes, by entity or domain.
/// `skip_domains` covers what HomeKit handles natively (media players).
pub fn missing_from_homekit(
    bridges: &[BridgeStatus],
    assist_exposed: &BTreeSet<String>,
    skip_domains: &[String],
) -> Vec<String> {
    let entities: BTreeSet<&str> = bridges
        .iter()
        .flat_map(|b| b.include_entities.iter().map(String::as_str))
        .collect();
    let domains: BTreeSet<&str> = bridges
        .iter()
        .flat_map(|b| b.include_domains.iter().map(String::as_str))
        .collect();
    assist_exposed
        .iter()
        .filter(|id| {
            let d = id.split_once('.').map_or("", |(d, _)| d);
            !skip_domains.iter().any(|s| s == d)
                && !entities.contains(id.as_str())
                && !domains.contains(d)
        })
        .cloned()
        .collect()
}

pub(crate) async fn audit(
    client: &Client,
    cfg: &crate::Config,
    skip_domains: &[String],
) -> Result<BridgeAudit> {
    let entries = client
        .get_json("/api/config/config_entries/entry?domain=homekit")
        .await?;
    let live: BTreeSet<String> = client
        .entity_list(None)
        .await?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["entity_id"].as_str().map(str::to_string))
        .collect();
    let mut bridges = Vec::new();
    let mut notes = Vec::new();
    for e in entries.as_array().into_iter().flatten() {
        let id = e["entry_id"].as_str().unwrap_or_default();
        // A failed diagnostics read is an error: reporting the bridge without
        // its filter would read as a clean bridge.
        let diag = client
            .get_json(&format!(
                "/api/diagnostics/config_entry/{}",
                plugin_toolkit::url::encode(id)
            ))
            .await
            .with_context(|| format!("HomeKit entry {id}: diagnostics unreadable"))?;
        bridges.push(bridge_status(e, Some(&diag), &live));
    }
    let failing = bridges
        .iter()
        .filter(|b| b.state != "loaded")
        .map(|b| b.entry_id.clone())
        .collect();
    let missing = match assist_exposed(cfg).await {
        Ok(exposed) => Some(missing_from_homekit(&bridges, &exposed, skip_domains)),
        Err(e) => {
            notes.push(format!("Assist parity skipped: {e}"));
            None
        }
    };
    Ok(BridgeAudit {
        bridges,
        failing,
        missing_from_homekit: missing,
        notes,
    })
}

async fn assist_exposed(cfg: &crate::Config) -> Result<BTreeSet<String>> {
    let mut ws = crate::ws::connect(cfg).await?;
    let list = ws
        .call("homeassistant/expose_entity/list", Value::Null)
        .await?;
    Ok(list["exposed_entities"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, m)| m["conversation"].as_bool() == Some(true))
        .map(|(id, _)| id.clone())
        .collect())
}

fn default_skip() -> Vec<String> {
    vec!["media_player".into()]
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
pub struct BridgeAuditArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    /// Domains excluded from Assist parity (HomeKit handles them natively).
    #[arg(long = "skip-domain", default_values_t = default_skip())]
    #[serde(default = "default_skip")]
    pub skip_domains: Vec<String>,
}

/// **Audit HomeKit bridges** (read-only): entries that are not loaded, include
/// filters naming deleted entities, and Assist-exposed entities missing from
/// every bridge.
#[orca_tool(domain = "home-assistant", verb = "bridge-audit")]
async fn ha_bridge_audit(args: BridgeAuditArgs, _ctx: &ToolCtx) -> Result<BridgeAudit> {
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let client = Client::new(cfg.clone());
    audit(&client, &cfg, &args.skip_domains).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn live() -> BTreeSet<String> {
        ["light.a", "switch.b", "media_player.tv"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn dead_refs_and_filter_location_variants() {
        let entry = json!({"entry_id": "e1", "title": "HASS Bridge", "state": "loaded"});
        for diag in [
            json!({"data": {"options": {"mode": "bridge", "filter": {"include_entities": ["light.a", "lock.door_lock"]}}}}),
            json!({"data": {"config-entry": {"options": {"mode": "bridge", "filter": {"include_entities": ["light.a", "lock.door_lock"]}}}}}),
        ] {
            let b = bridge_status(&entry, Some(&diag), &live());
            assert_eq!(b.dead_refs, ["lock.door_lock"]);
            assert_eq!(b.mode.as_deref(), Some("bridge"));
            assert!(!b.filter_unknown);
        }
        let b = bridge_status(&entry, None, &live());
        assert!(b.filter_unknown && b.dead_refs.is_empty());
    }

    #[test]
    fn parity_counts_entities_and_domains_and_skips_media() {
        let b = BridgeStatus {
            entry_id: "e".into(),
            title: "t".into(),
            state: "loaded".into(),
            reason: None,
            mode: None,
            include_entities: vec!["light.a".into()],
            include_domains: vec!["scene".into()],
            dead_refs: vec![],
            filter_unknown: false,
        };
        let exposed: BTreeSet<String> = ["light.a", "light.z", "scene.night", "media_player.tv"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            missing_from_homekit(&[b], &exposed, &default_skip()),
            ["light.z"]
        );
    }

    #[tokio::test]
    async fn audit_over_rest_flags_failing_and_dead_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/config/config_entries/entry"))
            .and(query_param("domain", "homekit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"entry_id": "e1", "title": "HASS Bridge", "state": "loaded"},
                {"entry_id": "e2", "title": "Frontdoor Lock:21069", "state": "setup_error", "reason": "entity not found"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/states"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"entity_id": "light.a"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/diagnostics/config_entry/e1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"options": {"filter": {"include_entities": ["light.a"]}}}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/diagnostics/config_entry/e2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"options": {"mode": "accessory", "filter": {"include_entities": ["lock.door_lock"]}}}}),
            ))
            .mount(&server)
            .await;
        let cfg = crate::Config::new(server.uri(), "tok");
        let a = audit(&Client::new(cfg.clone()), &cfg, &default_skip())
            .await
            .unwrap();
        assert_eq!(a.failing, ["e2"]);
        assert_eq!(a.bridges[1].dead_refs, ["lock.door_lock"]);
        assert!(a.bridges[0].dead_refs.is_empty());
        assert_eq!(a.missing_from_homekit, None);
        assert!(a.notes.iter().any(|n| n.contains("transport unavailable")));
    }

    async fn server_with_diag(status: u16, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/config/config_entries/entry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!([{"entry_id": "e1", "title": "HASS Bridge", "state": "loaded"}]),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/states"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/diagnostics/config_entry/e1"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn unreadable_diagnostics_fail_the_audit() {
        for (status, body) in [
            (401, "401: Unauthorized"),
            (404, "Not Found"),
            (200, "<html>"),
        ] {
            let server = server_with_diag(status, body).await;
            let cfg = crate::Config::new(server.uri(), "tok");
            let err = audit(&Client::new(cfg.clone()), &cfg, &default_skip())
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("diagnostics unreadable"),
                "{status}: {err}"
            );
        }
    }
}
