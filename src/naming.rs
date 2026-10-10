//! Naming rules for voice: alias collisions within an area, and aliases that
//! misbehave.
//!
//! HA parses "the <area> <noun>" as area + name, so names and aliases only
//! need to be unique within one area; the same bare noun in two areas is
//! correct. Two hazards are encoded:
//! - A bare plural alias ("lights") hijacks area-wide "turn on the X lights".
//! - HA shows an alias as a display name, so ultra-short ones ("bar") read
//!   badly ("Bar is idle").
#![allow(clippy::disallowed_types)]

use std::collections::BTreeMap;

use plugin_toolkit::prelude::*;

use crate::registry::{Area, RegistryEntity};

/// Aliases shorter than this read badly as display names.
pub const MIN_ALIAS_LEN: usize = 4;

/// Scope key for entities without an area.
pub const NO_AREA: &str = "(no area)";

/// Lowercase, collapse whitespace, drop a leading "the".
pub fn normalize(s: &str) -> String {
    let words: Vec<String> = s.split_whitespace().map(|w| w.to_lowercase()).collect();
    let words = match words.first().map(String::as_str) {
        Some("the") if words.len() > 1 => &words[1..],
        _ => &words[..],
    };
    words.join(" ")
}

/// A bare plural: one word ending in `s` but not `ss` or `us`.
pub fn is_bare_plural(alias: &str) -> bool {
    let n = normalize(alias);
    !n.contains(' ') && n.ends_with('s') && !n.ends_with("ss") && !n.ends_with("us") && n.len() > 3
}

fn voice_entity(e: &RegistryEntity) -> bool {
    e.hidden_by.is_none() && e.disabled_by.is_none()
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
pub struct Collision {
    pub area: String,
    /// The normalized phrase two or more entities answer to.
    pub phrase: String,
    pub entity_ids: Vec<String>,
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
pub struct AliasProblem {
    pub entity_id: String,
    pub alias: String,
    /// `plural` or `short`.
    pub kind: String,
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
pub struct NamingAudit {
    pub collisions: Vec<Collision>,
    pub alias_problems: Vec<AliasProblem>,
    /// Voice-visible entities with no area: "the <area> X" cannot reach them.
    pub missing_area: Vec<String>,
}

/// Every phrase an entity answers to, normalized: its name and its aliases.
pub fn phrases(e: &RegistryEntity) -> Vec<String> {
    let mut p: Vec<String> = e
        .display_name()
        .into_iter()
        .chain(e.aliases.iter().map(String::as_str))
        .map(normalize)
        .filter(|s| !s.is_empty())
        .collect();
    p.sort();
    p.dedup();
    p
}

/// Area id → display name, for reporting.
fn area_names(areas: &[Area]) -> BTreeMap<&str, &str> {
    areas
        .iter()
        .map(|a| (a.area_id.as_str(), a.name.as_str()))
        .collect()
}

pub fn audit(entities: &[RegistryEntity], areas: &[Area]) -> NamingAudit {
    let names = area_names(areas);
    let mut by_scope: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut out = NamingAudit::default();
    for e in entities.iter().filter(|e| voice_entity(e)) {
        let area = match &e.area_id {
            Some(a) => names.get(a.as_str()).copied().unwrap_or(a).to_string(),
            None => {
                out.missing_area.push(e.entity_id.clone());
                NO_AREA.to_string()
            }
        };
        for p in phrases(e) {
            by_scope
                .entry((area.clone(), p))
                .or_default()
                .push(e.entity_id.clone());
        }
        for a in &e.aliases {
            let kind = if is_bare_plural(a) {
                "plural"
            } else if normalize(a).chars().count() < MIN_ALIAS_LEN {
                "short"
            } else {
                continue;
            };
            out.alias_problems.push(AliasProblem {
                entity_id: e.entity_id.clone(),
                alias: a.clone(),
                kind: kind.into(),
            });
        }
    }
    out.collisions = by_scope
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|((area, phrase), mut ids)| {
            ids.sort();
            ids.dedup();
            Collision {
                area,
                phrase,
                entity_ids: ids,
            }
        })
        .filter(|c| c.entity_ids.len() > 1)
        .collect();
    out
}

/// **Audit voice names and aliases** (read-only): phrases two entities answer
/// to within one area, bare-plural and too-short aliases, and voice-visible
/// entities without an area.
#[endpoint_tool(domain = "home-assistant", verb = "naming-audit", resolve = crate::tools::endpoint_config)]
async fn ha_naming_audit(cfg: crate::Config) -> Result<NamingAudit> {
    let mut ws = crate::ws::connect(&cfg).await?;
    let entities = crate::registry::entities(&mut ws).await?;
    let areas = crate::registry::areas(&mut ws).await?;
    Ok(audit(&entities, &areas))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub(crate) fn ent(
        id: &str,
        name: &str,
        area: Option<&str>,
        aliases: &[&str],
    ) -> RegistryEntity {
        RegistryEntity {
            entity_id: id.into(),
            name: Some(name.into()),
            area_id: area.map(str::to_string),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    pub(crate) fn areas() -> Vec<Area> {
        ["bedroom", "living_room", "office"]
            .iter()
            .map(|id| Area {
                area_id: id.to_string(),
                name: match *id {
                    "living_room" => "Living Room".into(),
                    other => {
                        let mut c = other.chars();
                        c.next()
                            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                            .unwrap_or_default()
                    }
                },
                aliases: vec![],
                floor_id: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{areas, ent};
    use super::*;

    #[test]
    fn normalize_lowercases_and_drops_leading_the() {
        assert_eq!(normalize("  The  Bedroom TV "), "bedroom tv");
        assert_eq!(normalize("The"), "the");
    }

    #[test]
    fn plural_detection() {
        assert!(is_bare_plural("Lights"));
        assert!(is_bare_plural("lamps"));
        assert!(!is_bare_plural("glass"));
        assert!(!is_bare_plural("status"));
        assert!(!is_bare_plural("desk lamps"), "only bare plurals hijack");
        assert!(!is_bare_plural("tv"));
    }

    #[test]
    fn shadow_player_stealing_bedroom_tv_is_a_collision() {
        let es = [
            ent(
                "media_player.bedroom_tv",
                "Bedroom TV",
                Some("bedroom"),
                &["tv"],
            ),
            ent(
                "media_player.ma_bedroom",
                "MA Bedroom",
                Some("bedroom"),
                &["bedroom tv"],
            ),
            ent(
                "media_player.office_tv",
                "Office TV",
                Some("office"),
                &["tv"],
            ),
        ];
        let a = audit(&es, &areas());
        assert_eq!(
            a.collisions,
            [Collision {
                area: "Bedroom".into(),
                phrase: "bedroom tv".into(),
                entity_ids: vec![
                    "media_player.bedroom_tv".into(),
                    "media_player.ma_bedroom".into()
                ],
            }],
            "the shared `tv` alias across areas is correct and not reported"
        );
    }

    #[test]
    fn hidden_entities_do_not_collide_and_area_less_share_a_scope() {
        let mut hidden = ent("light.b", "Lamp", Some("office"), &[]);
        hidden.hidden_by = Some("user".into());
        let es = [
            ent("light.a", "Lamp", Some("office"), &[]),
            hidden,
            ent("light.x", "Strip", None, &[]),
            ent("light.y", "strip", None, &[]),
        ];
        let a = audit(&es, &areas());
        assert_eq!(a.collisions.len(), 1);
        assert_eq!(a.collisions[0].area, NO_AREA);
        assert_eq!(a.missing_area, ["light.x", "light.y"]);
    }

    #[test]
    fn flags_plural_and_short_aliases() {
        let es = [ent(
            "light.a",
            "Overhead",
            Some("office"),
            &["lights", "bar", "overhead light"],
        )];
        let a = audit(&es, &areas());
        let kinds: Vec<_> = a
            .alias_problems
            .iter()
            .map(|p| (p.alias.as_str(), p.kind.as_str()))
            .collect();
        assert_eq!(kinds, [("lights", "plural"), ("bar", "short")]);
    }
}
