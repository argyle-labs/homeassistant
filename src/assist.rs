//! Assist pipeline read and validation.
//!
//! A pipeline whose STT, TTS, conversation agent, or wake word points at an
//! entity that no longer exists fails every voice command at that stage
//! (`stt-provider-missing` when the Whisper add-on was removed), and nothing
//! reports it. [`validate`] checks every reference against live entities.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeSet;

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::Value;

use crate::ws::{HaWs, WsTransport};

/// Conversation agent ids HA provides without an entity.
const BUILTIN_AGENTS: &[&str] = &["homeassistant", "conversation.home_assistant"];

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
pub struct PipelineProblem {
    /// `stt_engine`, `tts_engine`, `conversation_engine`, or `wake_word_entity`.
    pub field: String,
    pub value: String,
    /// `missing`: names an entity that does not exist. `unverified`: a
    /// provider id that is not an entity, so existence cannot be checked here.
    pub kind: String,
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
pub struct PipelineStatus {
    pub id: String,
    pub name: String,
    pub preferred: bool,
    pub stt_engine: Option<String>,
    pub tts_engine: Option<String>,
    pub conversation_engine: Option<String>,
    pub wake_word_entity: Option<String>,
    pub problems: Vec<PipelineProblem>,
}

impl PipelineStatus {
    pub fn broken(&self) -> bool {
        self.problems.iter().any(|p| p.kind == "missing")
    }
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
pub struct PipelineReport {
    pub preferred_pipeline: Option<String>,
    /// The preferred pipeline is missing or has a `missing` reference: voice is down.
    pub preferred_broken: bool,
    pub pipelines: Vec<PipelineStatus>,
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn check(
    field: &str,
    value: &Option<String>,
    entities: &BTreeSet<String>,
    out: &mut Vec<PipelineProblem>,
) {
    let Some(v) = value else { return };
    if field == "conversation_engine" && BUILTIN_AGENTS.contains(&v.as_str()) {
        return;
    }
    let kind = if v.contains('.') {
        if entities.contains(v) {
            return;
        }
        "missing"
    } else {
        "unverified"
    };
    out.push(PipelineProblem {
        field: field.into(),
        value: v.clone(),
        kind: kind.into(),
    });
}

/// Validates an `assist_pipeline/pipeline/list` result against live entity ids.
pub fn validate(list: &Value, entities: &BTreeSet<String>) -> PipelineReport {
    let preferred = opt(&list["preferred_pipeline"]);
    let pipelines: Vec<PipelineStatus> = list["pipelines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = opt(&p["id"])?;
            let mut st = PipelineStatus {
                preferred: preferred.as_deref() == Some(id.as_str()),
                name: opt(&p["name"]).unwrap_or_default(),
                stt_engine: opt(&p["stt_engine"]),
                tts_engine: opt(&p["tts_engine"]),
                conversation_engine: opt(&p["conversation_engine"]),
                wake_word_entity: opt(&p["wake_word_entity"]),
                problems: Vec::new(),
                id,
            };
            let mut problems = Vec::new();
            check("stt_engine", &st.stt_engine, entities, &mut problems);
            check("tts_engine", &st.tts_engine, entities, &mut problems);
            check(
                "conversation_engine",
                &st.conversation_engine,
                entities,
                &mut problems,
            );
            check(
                "wake_word_entity",
                &st.wake_word_entity,
                entities,
                &mut problems,
            );
            st.problems = problems;
            Some(st)
        })
        .collect();
    let preferred_broken = match &preferred {
        None => false,
        Some(id) => pipelines
            .iter()
            .find(|p| &p.id == id)
            .is_none_or(PipelineStatus::broken),
    };
    PipelineReport {
        preferred_pipeline: preferred,
        preferred_broken,
        pipelines,
    }
}

pub(crate) async fn entity_ids<T: WsTransport>(ws: &mut HaWs<T>) -> Result<BTreeSet<String>> {
    let states = ws.call("get_states", Value::Null).await?;
    Ok(states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| opt(&s["entity_id"]))
        .collect())
}

/// **List Assist pipelines and flag broken references** (read-only): any
/// STT/TTS/conversation/wake-word reference to an entity that does not exist,
/// and whether the preferred pipeline is affected.
#[endpoint_tool(domain = "home-assistant", verb = "assist-pipelines", resolve = crate::tools::endpoint_config)]
async fn ha_assist_pipelines(cfg: crate::Config) -> Result<PipelineReport> {
    let mut ws = crate::ws::connect(&cfg).await?;
    let entities = entity_ids(&mut ws).await?;
    let list = ws
        .call("assist_pipeline/pipeline/list", Value::Null)
        .await?;
    Ok(validate(&list, &entities))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::testing::FakeHa;
    use plugin_toolkit::serde_json::json;

    fn ents(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn list(preferred: &str, stt: &str) -> Value {
        json!({
            "preferred_pipeline": preferred,
            "pipelines": [
                {"id": "p1", "name": "Home", "stt_engine": stt, "tts_engine": "tts.piper",
                 "conversation_engine": "conversation.home_assistant", "wake_word_entity": null},
                {"id": "p2", "name": "Text", "stt_engine": null, "tts_engine": null,
                 "conversation_engine": "homeassistant"}
            ]
        })
    }

    #[test]
    fn missing_stt_entity_breaks_the_preferred_pipeline() {
        let r = validate(&list("p1", "stt.faster_whisper"), &ents(&["tts.piper"]));
        assert!(r.preferred_broken);
        assert_eq!(
            r.pipelines[0].problems,
            [PipelineProblem {
                field: "stt_engine".into(),
                value: "stt.faster_whisper".into(),
                kind: "missing".into()
            }]
        );
        assert!(
            r.pipelines[1].problems.is_empty(),
            "text-only pipeline is fine"
        );
    }

    #[test]
    fn healthy_pipeline_has_no_problems() {
        let r = validate(
            &list("p1", "stt.whisper"),
            &ents(&["stt.whisper", "tts.piper"]),
        );
        assert!(!r.preferred_broken);
        assert!(r.pipelines.iter().all(|p| p.problems.is_empty()));
    }

    #[test]
    fn provider_ids_are_unverified_not_missing() {
        let r = validate(&list("p1", "cloud"), &ents(&["tts.piper"]));
        assert_eq!(r.pipelines[0].problems[0].kind, "unverified");
        assert!(!r.preferred_broken);
    }

    #[test]
    fn preferred_pipeline_that_does_not_exist_is_broken() {
        let r = validate(
            &list("gone", "stt.whisper"),
            &ents(&["stt.whisper", "tts.piper"]),
        );
        assert!(r.preferred_broken);
    }

    #[test]
    fn missing_wake_word_entity_is_flagged() {
        let l = json!({"pipelines": [{"id": "p", "wake_word_entity": "wake_word.openwakeword"}]});
        let r = validate(&l, &ents(&[]));
        assert_eq!(r.pipelines[0].problems[0].field, "wake_word_entity");
    }

    #[tokio::test]
    async fn reads_states_and_pipelines_over_ws() {
        let mut ws = FakeHa::new()
            .on(
                "get_states",
                |_| json!([{"entity_id": "tts.piper"}, {"entity_id": "stt.whisper"}]),
            )
            .on("assist_pipeline/pipeline/list", |_| {
                list("p1", "stt.whisper")
            })
            .session()
            .await;
        let e = entity_ids(&mut ws).await.unwrap();
        let l = ws
            .call("assist_pipeline/pipeline/list", Value::Null)
            .await
            .unwrap();
        assert!(!validate(&l, &e).preferred_broken);
    }
}
