//! Voice-naming audit: flags entity names that Assist / HomeKit / Google voice
//! surfaces will mishear, confuse, or act on dangerously.
//!
//! Pure over the `/api/states` payload, so it reads nothing beyond what
//! `home-assistant.entities` already returns and never calls a service.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeMap;

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::Value;

use crate::tools::make_client;

/// Domains a voice command can actuate. Sensors and other read-only entities
/// are skipped: a poor sensor name is noise, not a misfire.
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

/// Names that say what a device is but not where or which one.
const GENERIC: &[&str] = &[
    "area", "dimmer", "fan", "lamp", "light", "lights", "lock", "main", "outlet", "overhead",
    "plug", "scene", "screen", "speaker", "switch", "tv",
];

/// Name fragments marking a load whose accidental "turn off" takes down
/// infrastructure or media playback.
const POWER_CRITICAL: &[&str] = &[
    "server",
    "media center",
    "media console",
    "router",
    "modem",
    "gateway",
    "nas",
    "network",
    "rack",
    "ups",
];

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "snake_case")]
pub enum VoiceIssue {
    /// No `friendly_name`; voice falls back to the entity id.
    MissingName,
    /// Name is a bare device word ("Speaker", "Overhead").
    Ambiguous,
    /// Another controllable entity has the same name (case-insensitive).
    Duplicate,
    /// Name carries an id fragment: an underscore or a trailing serial number.
    LeakedSlug,
    /// Name or entity id marks power-critical load; keep it out of voice exposure.
    PowerCritical,
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
pub struct VoiceFinding {
    pub entity_id: String,
    pub name: Option<String>,
    pub issues: Vec<VoiceIssue>,
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
pub struct VoiceAudit {
    /// Controllable entities examined.
    pub checked: usize,
    /// Entities with at least one issue, sorted by entity id.
    pub findings: Vec<VoiceFinding>,
}

fn domain_of(entity_id: &str) -> &str {
    entity_id.split_once('.').map_or("", |(d, _)| d)
}

fn is_generic(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    GENERIC.contains(&lower.as_str())
}

fn has_leaked_slug(name: &str) -> bool {
    if name.contains('_') {
        return true;
    }
    // A word mixing letters with a 3+ digit run ("googlehome7284") is a serial,
    // not a name; "Bedroom 2" is fine.
    name.split_whitespace().any(|w| {
        let digits = w.chars().filter(char::is_ascii_digit).count();
        digits >= 3 && w.chars().any(|c| c.is_ascii_alphabetic())
    })
}

fn is_power_critical(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    POWER_CRITICAL.iter().any(|frag| {
        lower
            .match_indices(frag)
            .any(|(i, _)| word_bounded(&lower, i, frag.len()))
    })
}

fn word_bounded(s: &str, start: usize, len: usize) -> bool {
    let before = s[..start].chars().next_back();
    let after = s[start + len..].chars().next();
    // A trailing "s" keeps plurals ("Servers") matching.
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric());
    edge(before)
        && (edge(after) || (after == Some('s') && edge(s[start + len + 1..].chars().next())))
}

/// Audits a `/api/states` array. Non-array input or entries without an
/// `entity_id` are skipped.
pub fn audit(states: &Value) -> VoiceAudit {
    let entities: Vec<(String, Option<String>)> = states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s.get("entity_id")?.as_str()?.to_string();
            if !CONTROLLABLE.contains(&domain_of(&id)) {
                return None;
            }
            let name = s
                .pointer("/attributes/friendly_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_string);
            Some((id, name))
        })
        .collect();

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for name in entities.iter().filter_map(|(_, n)| n.as_ref()) {
        *counts.entry(name.to_lowercase()).or_default() += 1;
    }

    let mut findings: Vec<VoiceFinding> = entities
        .iter()
        .filter_map(|(id, name)| {
            let mut issues = Vec::new();
            match name {
                None => issues.push(VoiceIssue::MissingName),
                Some(n) => {
                    if is_generic(n) {
                        issues.push(VoiceIssue::Ambiguous);
                    }
                    if counts.get(&n.to_lowercase()).copied().unwrap_or(0) > 1 {
                        issues.push(VoiceIssue::Duplicate);
                    }
                    if has_leaked_slug(n) {
                        issues.push(VoiceIssue::LeakedSlug);
                    }
                }
            }
            // The id catches a renamed load ("switch.office_servers" called "Desk").
            let object_id = id.split_once('.').map_or("", |(_, o)| o).replace('_', " ");
            if is_power_critical(&object_id) || name.as_deref().is_some_and(is_power_critical) {
                issues.push(VoiceIssue::PowerCritical);
            }
            (!issues.is_empty()).then(|| VoiceFinding {
                entity_id: id.clone(),
                name: name.clone(),
                issues,
            })
        })
        .collect();
    findings.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));

    VoiceAudit {
        checked: entities.len(),
        findings,
    }
}

/// **Audit entity names for voice control.** Read-only: lists controllable
/// entities whose names are missing, ambiguous, duplicated, carry id slugs, or
/// mark power-critical switches that should not be exposed to voice.
#[endpoint_tool(domain = "home-assistant", verb = "voice-audit")]
async fn ha_voice_audit(client: Client) -> Result<VoiceAudit> {
    Ok(audit(&client.entity_list(None).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::json;

    fn state(id: &str, name: Option<&str>) -> Value {
        match name {
            Some(n) => json!({"entity_id": id, "attributes": {"friendly_name": n}}),
            None => json!({"entity_id": id, "attributes": {}}),
        }
    }

    fn issues_for<'a>(a: &'a VoiceAudit, id: &str) -> &'a [VoiceIssue] {
        a.findings
            .iter()
            .find(|f| f.entity_id == id)
            .map_or(&[], |f| f.issues.as_slice())
    }

    #[test]
    fn skips_read_only_domains() {
        let a = audit(&json!([
            state("sensor.x", None),
            state("weather.y", Some("Speaker"))
        ]));
        assert_eq!(a.checked, 0);
        assert!(a.findings.is_empty());
    }

    #[test]
    fn flags_missing_and_generic_names() {
        let a = audit(&json!([
            state("light.a", None),
            state("media_player.googlehome7284", Some("Speaker")),
            state("light.office_table_lamp", Some("Office Table Lamp")),
        ]));
        assert_eq!(a.checked, 3);
        assert_eq!(issues_for(&a, "light.a"), [VoiceIssue::MissingName]);
        assert_eq!(
            issues_for(&a, "media_player.googlehome7284"),
            [VoiceIssue::Ambiguous]
        );
        assert!(issues_for(&a, "light.office_table_lamp").is_empty());
    }

    #[test]
    fn flags_case_insensitive_duplicates() {
        let a = audit(&json!([
            state("switch.office_overhead", Some("Overhead")),
            state("switch.bedroom_main_lights", Some("overhead")),
        ]));
        for id in ["switch.office_overhead", "switch.bedroom_main_lights"] {
            assert_eq!(
                issues_for(&a, id),
                [VoiceIssue::Ambiguous, VoiceIssue::Duplicate]
            );
        }
    }

    #[test]
    fn flags_leaked_slugs_but_not_room_numbers() {
        assert!(has_leaked_slug("Washer Kitchen-Washer_switch"));
        assert!(has_leaked_slug("googlehome7284"));
        assert!(!has_leaked_slug("Bedroom 2 Lamp"));
        assert!(!has_leaked_slug("Living Room TV"));
    }

    #[test]
    fn flags_power_critical_in_any_domain_by_name_or_id() {
        let a = audit(&json!([
            state("switch.office_servers", Some("Servers")),
            state("switch.living_room_media_console", Some("Media Center")),
            state("light.nas_closet", Some("Closet")),
            state("input_boolean.rack_power", Some("Desk Toggle")),
            state("switch.desk", Some("Network Switch")),
            state("switch.cupsboard_lamp", Some("Cupsboard Lamp")),
            state("light.kitchen", Some("Kitchen")),
        ]));
        for id in [
            "switch.office_servers",
            "switch.living_room_media_console",
            "light.nas_closet",
            "input_boolean.rack_power",
            "switch.desk",
        ] {
            assert_eq!(issues_for(&a, id), [VoiceIssue::PowerCritical], "{id}");
        }
        assert!(issues_for(&a, "switch.cupsboard_lamp").is_empty());
        assert!(issues_for(&a, "light.kitchen").is_empty());
    }

    #[test]
    fn findings_are_sorted_and_input_tolerant() {
        let a = audit(&json!([
            state("switch.z", None),
            {"no_id": true},
            state("light.a", None),
        ]));
        let ids: Vec<_> = a.findings.iter().map(|f| f.entity_id.as_str()).collect();
        assert_eq!(ids, ["light.a", "switch.z"]);
        assert_eq!(audit(&json!({"not": "an array"})).checked, 0);
    }
}
