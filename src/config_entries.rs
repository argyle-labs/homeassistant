//! Config-entry drift: duplicate entries and entries that failed to load.
//!
//! A device re-discovered at a new address can be added again beside its old
//! entry (four stale Songpal entries for two devices). Duplicates share a
//! domain and title; the stale one usually sits in `setup_retry`.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeMap;

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::Value;

use crate::tools::make_client;

/// States that mean an enabled entry is not working.
const FAILED_STATES: &[&str] = &[
    "setup_error",
    "setup_retry",
    "not_loaded",
    "failed_unload",
    "migration_error",
    "setup_in_progress",
];

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
pub struct EntryRef {
    pub entry_id: String,
    pub domain: String,
    pub title: String,
    pub state: String,
    pub reason: Option<String>,
    pub disabled_by: Option<String>,
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
pub struct EntryDrift {
    pub total: usize,
    /// Groups of two or more entries with the same domain and title.
    pub duplicates: Vec<Vec<EntryRef>>,
    /// Enabled entries in a failed or not-loaded state.
    pub failing: Vec<EntryRef>,
}

fn entry_ref(e: &Value) -> Option<EntryRef> {
    let s = |k: &str| e[k].as_str().filter(|v| !v.is_empty()).map(str::to_string);
    Some(EntryRef {
        entry_id: s("entry_id")?,
        domain: s("domain")?,
        title: s("title").unwrap_or_default(),
        state: s("state").unwrap_or_else(|| "unknown".into()),
        reason: s("reason"),
        disabled_by: s("disabled_by"),
    })
}

pub fn drift(entries: &Value) -> EntryDrift {
    let refs: Vec<EntryRef> = entries
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(entry_ref)
        .collect();
    let mut groups: BTreeMap<(String, String), Vec<EntryRef>> = BTreeMap::new();
    for r in &refs {
        // An empty title says nothing about identity; never group on it.
        if r.title.trim().is_empty() {
            continue;
        }
        groups
            .entry((r.domain.clone(), r.title.trim().to_lowercase()))
            .or_default()
            .push(r.clone());
    }
    EntryDrift {
        total: refs.len(),
        duplicates: groups.into_values().filter(|g| g.len() > 1).collect(),
        failing: refs
            .iter()
            .filter(|r| r.disabled_by.is_none() && FAILED_STATES.contains(&r.state.as_str()))
            .cloned()
            .collect(),
    }
}

/// **Find config-entry drift** (read-only): duplicate entries (same domain
/// and title) and enabled entries that failed to load.
#[endpoint_tool(domain = "home-assistant", verb = "config-entry-drift")]
async fn ha_config_entry_drift(client: crate::Client) -> Result<EntryDrift> {
    Ok(drift(
        &client.get_json("/api/config/config_entries/entry").await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::json;

    #[test]
    fn groups_duplicates_and_lists_failures() {
        let d = drift(&json!([
            {"entry_id": "1", "domain": "songpal", "title": "Living Room Soundbar", "state": "loaded"},
            {"entry_id": "2", "domain": "songpal", "title": "living room soundbar ", "state": "setup_retry"},
            {"entry_id": "3", "domain": "songpal", "title": "Bedroom", "state": "loaded"},
            {"entry_id": "4", "domain": "hue", "title": "Bedroom", "state": "loaded"},
            {"entry_id": "5", "domain": "zha", "title": "", "state": "loaded"},
            {"entry_id": "6", "domain": "zha", "title": "", "state": "loaded"},
            {"entry_id": "7", "domain": "cast", "title": "Old", "state": "not_loaded", "disabled_by": "user"},
            {"entry_id": "8", "domain": "met", "title": "Home", "state": "setup_error", "reason": "timeout"},
            {"no_id": true}
        ]));
        assert_eq!(d.total, 8);
        assert_eq!(
            d.duplicates.len(),
            1,
            "same title in another domain is not a duplicate"
        );
        let ids: Vec<_> = d.duplicates[0]
            .iter()
            .map(|e| e.entry_id.as_str())
            .collect();
        assert_eq!(ids, ["1", "2"]);
        let failing: Vec<_> = d.failing.iter().map(|e| e.entry_id.as_str()).collect();
        assert_eq!(failing, ["2", "8"], "disabled entries are not failures");
        assert_eq!(d.failing[1].reason.as_deref(), Some("timeout"));
    }
}
