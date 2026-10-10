//! Bare-noun alias generation.
//!
//! HA reads "the living room overhead" as area "living room" + name
//! "overhead", so an entity named "Living Room Overhead" is unreachable that
//! way unless it also carries the bare noun "overhead" as an alias. The
//! generator proposes that noun and skips anything the naming rules forbid.
#![allow(clippy::disallowed_types)]

use std::collections::{BTreeMap, BTreeSet};

use plugin_toolkit::prelude::*;

use crate::naming::{is_bare_plural, normalize, phrases, MIN_ALIAS_LEN};
use crate::registry::{update_verified, Area, RegistryChange, RegistryEntity};
use plugin_toolkit::serde_json::json;

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
pub struct Suggestion {
    pub entity_id: String,
    pub area: String,
    pub alias: String,
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
pub struct Skipped {
    pub entity_id: String,
    pub noun: String,
    /// `plural`, `short`, `collision`, or `present`.
    pub reason: String,
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
pub struct AliasPlan {
    pub suggestions: Vec<Suggestion>,
    pub skipped: Vec<Skipped>,
}

/// The name with the area's name (or an area alias) removed from the front or
/// back, normalized. `None` when the name does not mention the area.
pub fn bare_noun(name: &str, area_phrases: &[String]) -> Option<String> {
    let n = normalize(name);
    let mut longest_first: Vec<&String> = area_phrases.iter().collect();
    longest_first.sort_by_key(|p| std::cmp::Reverse(p.len()));
    for a in longest_first {
        let rest = n
            .strip_prefix(&format!("{a} "))
            .or_else(|| n.strip_suffix(&format!(" {a}")));
        if let Some(r) = rest.map(str::trim).filter(|r| !r.is_empty()) {
            return Some(r.to_string());
        }
    }
    None
}

pub fn plan(entities: &[RegistryEntity], areas: &[Area]) -> AliasPlan {
    let area_phrases: BTreeMap<&str, Vec<String>> = areas
        .iter()
        .map(|a| {
            let mut p: Vec<String> = std::iter::once(&a.name)
                .chain(a.aliases.iter())
                .map(|s| normalize(s))
                .collect();
            p.push(normalize(&a.area_id.replace('_', " ")));
            p.sort();
            p.dedup();
            (a.area_id.as_str(), p)
        })
        .collect();
    let area_names: BTreeMap<&str, &str> = areas
        .iter()
        .map(|a| (a.area_id.as_str(), a.name.as_str()))
        .collect();
    let visible: Vec<&RegistryEntity> = entities
        .iter()
        .filter(|e| e.hidden_by.is_none() && e.disabled_by.is_none())
        .collect();

    // Phrases already taken per area, including every candidate noun, so two
    // entities proposing the same noun in one area both get skipped.
    let mut taken: BTreeMap<&str, BTreeMap<String, BTreeSet<&str>>> = BTreeMap::new();
    let mut candidates: Vec<(&RegistryEntity, &str, String)> = Vec::new();
    for e in &visible {
        let Some(area) = e.area_id.as_deref() else {
            continue;
        };
        for p in phrases(e) {
            taken
                .entry(area)
                .or_default()
                .entry(p)
                .or_default()
                .insert(&e.entity_id);
        }
        let Some(name) = e.display_name() else {
            continue;
        };
        if let Some(noun) = bare_noun(name, area_phrases.get(area).map_or(&[][..], |v| v)) {
            taken
                .entry(area)
                .or_default()
                .entry(noun.clone())
                .or_default()
                .insert(&e.entity_id);
            candidates.push((e, area, noun));
        }
    }

    let mut out = AliasPlan::default();
    for (e, area, noun) in candidates {
        let skip = |reason: &str| Skipped {
            entity_id: e.entity_id.clone(),
            noun: noun.clone(),
            reason: reason.into(),
        };
        if e.aliases.iter().any(|a| normalize(a) == noun) {
            out.skipped.push(skip("present"));
        } else if is_bare_plural(&noun) {
            out.skipped.push(skip("plural"));
        } else if noun.chars().count() < MIN_ALIAS_LEN {
            out.skipped.push(skip("short"));
        } else if taken[area][&noun].len() > 1 {
            out.skipped.push(skip("collision"));
        } else {
            out.suggestions.push(Suggestion {
                entity_id: e.entity_id.clone(),
                area: area_names.get(area).copied().unwrap_or(area).to_string(),
                alias: noun,
            });
        }
    }
    out
}

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct AliasArgs {
    /// Registered endpoint name.
    #[arg(long)]
    pub endpoint: String,
    /// Add the suggested aliases. Without it the call returns the plan only.
    #[arg(long)]
    #[serde(default)]
    pub execute: bool,
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
pub struct AliasOutput {
    pub dry_run: bool,
    #[serde(flatten)]
    pub plan: AliasPlan,
    /// Entities whose alias was written and verified.
    pub applied: Vec<String>,
    /// `entity_id: error` for writes that failed or did not stick.
    pub failed: Vec<String>,
    /// False when any write failed.
    pub success: bool,
}

/// The entity's aliases as stored right now, so a write never drops an alias
/// added since the plan was read.
async fn current_aliases<T: crate::ws::WsTransport>(
    ws: &mut crate::ws::HaWs<T>,
    entity_id: &str,
) -> Result<Vec<String>> {
    let e = ws
        .call(
            "config/entity_registry/get",
            json!({"entity_id": entity_id}),
        )
        .await?;
    crate::registry::from_entry(&e)
        .map(|e| e.aliases)
        .with_context(|| format!("{entity_id} is no longer in the registry"))
}

pub(crate) async fn apply<T: crate::ws::WsTransport>(
    ws: &mut crate::ws::HaWs<T>,
    plan: AliasPlan,
    execute: bool,
) -> AliasOutput {
    let mut out = AliasOutput {
        dry_run: !execute,
        plan,
        applied: Vec::new(),
        failed: Vec::new(),
        success: true,
    };
    if !execute {
        return out;
    }
    for s in &out.plan.suggestions {
        let r = async {
            let current = current_aliases(ws, &s.entity_id).await?;
            if current.iter().any(|a| normalize(a) == s.alias) {
                return Ok(());
            }
            let change = RegistryChange {
                aliases: Some(current.into_iter().chain([s.alias.clone()]).collect()),
                ..Default::default()
            };
            update_verified(ws, &s.entity_id, &change).await.map(|_| ())
        }
        .await;
        match r {
            Ok(()) => out.applied.push(s.entity_id.clone()),
            Err(e) => out.failed.push(format!("{}: {e:#}", s.entity_id)),
        }
    }
    out.success = out.failed.is_empty();
    out
}

/// **Suggest and add bare-noun aliases** [MUTATES STATE]. Requires admin, for
/// dry runs too. Dry-run unless `execute`: returns each entity's proposed bare
/// noun and why others were skipped (plural, short, collision, present). Each
/// executed write keeps existing aliases and is verified by re-reading.
#[orca_tool(domain = "home-assistant", verb = "naming-aliases", role = "admin")]
async fn ha_naming_aliases(args: AliasArgs, ctx: &ToolCtx) -> Result<AliasOutput> {
    crate::authz::require_admin(ctx)?;
    let cfg = crate::tools::endpoint_config(&args.endpoint)?;
    let mut ws = crate::ws::connect(&cfg).await?;
    let entities = crate::registry::entities(&mut ws).await?;
    let areas = crate::registry::areas(&mut ws).await?;
    let p = plan(&entities, &areas);
    let out = apply(&mut ws, p, args.execute).await;
    if !out.success {
        bail!(
            "{} alias write(s) failed: {}; applied: {}",
            out.failed.len(),
            out.failed.join("; "),
            if out.applied.is_empty() {
                "none".into()
            } else {
                out.applied.join(", ")
            }
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::naming::fixtures::{areas, ent};
    use crate::ws::testing::FakeHa;
    use plugin_toolkit::serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[test]
    fn bare_noun_strips_area_at_either_end() {
        let lr = vec!["living room".to_string()];
        assert_eq!(
            bare_noun("Living Room Overhead", &lr).as_deref(),
            Some("overhead")
        );
        assert_eq!(
            bare_noun("Overhead Living Room", &lr).as_deref(),
            Some("overhead")
        );
        assert_eq!(bare_noun("Overhead", &lr), None);
        assert_eq!(bare_noun("Living Room", &lr), None);
    }

    #[test]
    fn plan_applies_every_rule() {
        let es = [
            ent(
                "light.lr_overhead",
                "Living Room Overhead",
                Some("living_room"),
                &[],
            ),
            ent(
                "light.lr_lights",
                "Living Room Lights",
                Some("living_room"),
                &[],
            ),
            ent("light.lr_bar", "Living Room Bar", Some("living_room"), &[]),
            ent(
                "light.office_lamp",
                "Office Lamp",
                Some("office"),
                &["lamp"],
            ),
            ent("light.bed_lamp", "Bedroom Lamp", Some("bedroom"), &[]),
            ent("light.bed_lamp2", "Lamp Bedroom", Some("bedroom"), &[]),
            ent(
                "media_player.bedroom_tv",
                "Bedroom TV",
                Some("bedroom"),
                &[],
            ),
        ];
        let p = plan(&es, &areas());
        let got: Vec<_> = p
            .suggestions
            .iter()
            .map(|s| (s.entity_id.as_str(), s.alias.as_str()))
            .collect();
        assert_eq!(got, [("light.lr_overhead", "overhead")]);
        let skipped: BTreeMap<_, _> = p
            .skipped
            .iter()
            .map(|s| (s.entity_id.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(skipped["light.lr_lights"], "plural");
        assert_eq!(skipped["light.lr_bar"], "short");
        assert_eq!(skipped["light.office_lamp"], "present");
        assert_eq!(skipped["light.bed_lamp"], "collision");
        assert_eq!(skipped["light.bed_lamp2"], "collision");
        assert_eq!(skipped["media_player.bedroom_tv"], "short");
    }

    #[test]
    fn same_noun_in_two_areas_is_allowed() {
        let es = [
            ent("light.a", "Office Overhead", Some("office"), &[]),
            ent("light.b", "Bedroom Overhead", Some("bedroom"), &[]),
        ];
        assert_eq!(plan(&es, &areas()).suggestions.len(), 2);
    }

    #[tokio::test]
    async fn apply_is_dry_run_by_default_and_verifies_writes() {
        let es = vec![ent(
            "light.lr_overhead",
            "Living Room Overhead",
            Some("living_room"),
            &["ceiling"],
        )];
        let p = plan(&es, &areas());
        let stored = Arc::new(Mutex::new(json!(["ceiling"])));
        let (w, r) = (Arc::clone(&stored), Arc::clone(&stored));
        let ha = FakeHa::new()
            .on("config/entity_registry/update", move |v| {
                *w.lock().unwrap() = v["aliases"].clone();
                Value::Null
            })
            .on("config/entity_registry/get", move |_| {
                json!({"entity_id": "light.lr_overhead", "aliases": r.lock().unwrap().clone()})
            });
        let seen = Arc::clone(&ha.seen);
        let mut ws = ha.session().await;

        let dry = apply(&mut ws, p.clone(), false).await;
        assert!(dry.dry_run && dry.applied.is_empty());
        assert!(seen.lock().unwrap().is_empty(), "a dry run sends nothing");

        // An alias added concurrently after the plan was read survives.
        *stored.lock().unwrap() = json!(["ceiling", "big light"]);
        let done = apply(&mut ws, p, true).await;
        assert_eq!(done.applied, ["light.lr_overhead"]);
        assert!(done.failed.is_empty() && done.success);
        assert_eq!(
            *stored.lock().unwrap(),
            json!(["big light", "ceiling", "overhead"])
        );
    }

    #[tokio::test]
    async fn failed_write_marks_the_run_unsuccessful() {
        let es = vec![ent(
            "light.lr_overhead",
            "Living Room Overhead",
            Some("living_room"),
            &[],
        )];
        let p = plan(&es, &areas());
        let mut ws = FakeHa::new()
            .on("config/entity_registry/update", |_| Value::Bool(false))
            .on(
                "config/entity_registry/get",
                |_| json!({"entity_id": "light.lr_overhead", "aliases": [null]}),
            )
            .session()
            .await;
        let out = apply(&mut ws, p, true).await;
        assert!(!out.success);
        assert_eq!(out.failed.len(), 1);
        assert!(out.failed[0].contains("not applied"), "{:?}", out.failed);
    }

    #[tokio::test]
    async fn tool_refuses_non_admin_before_planning() {
        use crate::authz::testing::{assert_refused, ctx};
        for c in [ctx(None), ctx(Some("user"))] {
            let args = AliasArgs {
                endpoint: "nope".into(),
                execute: false,
            };
            assert_refused(&ha_naming_aliases(args, &c).await.unwrap_err());
        }
    }
}
