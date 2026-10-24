#![allow(clippy::disallowed_types)]

use plugin_toolkit::abi::{WsChunk, WsOpen};
use plugin_toolkit::contract::backup::BackupRecord;
use plugin_toolkit::delegated_ws::WsClient;
use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Value};
use plugin_toolkit::time::Timestamp;

use std::time::{Duration, Instant};

use crate::Client;

pub const RECORD_KIND: &str = "home-assistant";

const ON_BOX_AGENTS: &[&str] = &["hassio.local", "backup.local"];

const MAX_MESSAGE_BYTES: u64 = 16 << 20;

const MAX_SKIPPED_MESSAGES: usize = 256;

const WS_BUDGET: Duration = Duration::from_secs(60);

const MAX_UPSTREAM_TEXT: usize = 64;

const FUTURE_SKEW_MS: i64 = 5 * 60_000;

pub fn upstream_text(v: &Value) -> String {
    let raw = raw_text(v);
    let clean: String = strip_ansi(&raw)
        .chars()
        .filter(|c| !is_hidden_char(*c))
        .collect();
    let scrubbed = plugin_toolkit::scrub::scrub_line(&clean).into_owned();
    scrubbed.chars().take(MAX_UPSTREAM_TEXT).collect()
}

fn is_hidden_char(c: char) -> bool {
    c.is_control()
        || ('\u{200B}'..='\u{200F}').contains(&c)
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
        || matches!(c, '\u{061C}' | '\u{FEFF}' | '\u{2028}' | '\u{2029}')
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for f in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&f) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn raw_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn is_off_box(agent_id: &str) -> bool {
    !ON_BOX_AGENTS.contains(&agent_id)
}

fn ws_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let rest = base
        .strip_prefix("https://")
        .map(|r| format!("wss://{r}"))
        .or_else(|| base.strip_prefix("http://").map(|r| format!("ws://{r}")))
        .unwrap_or_else(|| base.to_string());
    format!("{rest}/api/websocket")
}

async fn recv_json(ws: &WsClient, deadline: Instant) -> Result<Value> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < Duration::from_millis(1) {
        bail!("home assistant did not reply before the deadline");
    }
    let chunk = ws
        .recv_timeout(remaining)
        .await
        .context("home assistant did not reply before the deadline")?
        .context("home assistant closed the websocket")?;
    let WsChunk::Text { text } = chunk else {
        bail!("home assistant sent a non-text websocket message");
    };
    Ok(plugin_toolkit::serde_json::from_str(&text)?)
}

impl Client {
    pub async fn ws_commands(&self, types: &[&str]) -> Result<Vec<Value>> {
        self.ws_commands_within(types, WS_BUDGET).await
    }

    async fn ws_commands_within(&self, types: &[&str], budget: Duration) -> Result<Vec<Value>> {
        let msgs: Vec<Value> = types.iter().map(|ty| json!({"type": ty})).collect();
        self.ws_requests_within(&msgs, budget).await
    }

    pub async fn ws_requests(&self, msgs: &[Value]) -> Result<Vec<Value>> {
        self.ws_requests_within(msgs, WS_BUDGET).await
    }

    async fn ws_requests_within(&self, msgs: &[Value], budget: Duration) -> Result<Vec<Value>> {
        let deadline = Instant::now() + budget;
        let ws = WsClient::connect(WsOpen {
            url: ws_url(&self.cfg.base_url),
            headers: Vec::new(),
            insecure: false,
            timeout_ms: None,
            max_message_bytes: Some(MAX_MESSAGE_BYTES),
        })
        .await?;
        let hello = recv_json(&ws, deadline).await?;
        if hello["type"] != "auth_required" {
            bail!(
                "unexpected websocket greeting: {}",
                upstream_text(&hello["type"])
            );
        }
        ws.send(json!({"type": "auth", "access_token": self.cfg.token}).to_string())
            .await?;
        let auth = recv_json(&ws, deadline).await?;
        if auth["type"] != "auth_ok" {
            bail!(
                "home assistant rejected the token ({})",
                upstream_text(&auth["type"])
            );
        }
        let mut out = Vec::with_capacity(msgs.len());
        for (i, msg) in msgs.iter().enumerate() {
            let id = i as u64 + 1;
            let mut req = msg.as_object().cloned().unwrap_or_default();
            req.insert("id".into(), json!(id));
            let req = Value::Object(req);
            let ty = match msg["endpoint"].as_str() {
                Some(ep) => format!("{} {ep}", raw_text(&msg["type"])),
                None => raw_text(&msg["type"]),
            };
            ws.send(req.to_string()).await?;
            let mut skipped = 0;
            let reply = loop {
                let msg = recv_json(&ws, deadline).await?;
                if msg["id"] == id && msg["type"] == "result" {
                    break msg;
                }
                skipped += 1;
                if skipped > MAX_SKIPPED_MESSAGES {
                    bail!("{ty}: more than {MAX_SKIPPED_MESSAGES} unrelated messages before the reply");
                }
            };
            if reply["success"] != true {
                bail!("{ty} failed: {}", upstream_text(&reply["error"]["message"]));
            }
            out.push(reply["result"].clone());
        }
        ws.close().await?;
        Ok(out)
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "lowercase")]
pub enum BackupType {
    Full,
    Partial,
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
pub struct BackupCopy {
    pub agent_id: String,
    pub protected: bool,
    pub size_bytes: u64,
    pub off_box: bool,
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
pub struct HaBackup {
    pub backup_id: String,
    pub name: String,
    pub date: String,
    pub created_ms: i64,
    #[serde(rename = "type")]
    pub backup_type: BackupType,
    pub protected: bool,
    pub size_bytes: u64,
    pub homeassistant_version: Option<String>,
    pub automatic: bool,
    pub copies: Vec<BackupCopy>,
    pub failed_agent_ids: Vec<String>,
    pub record: BackupRecord,
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
pub struct HaBackupList {
    pub backups: Vec<HaBackup>,
    pub agents: Vec<String>,
    pub agent_errors: Vec<AgentError>,
    pub last_attempted_automatic_backup: Option<String>,
    pub last_completed_automatic_backup: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
pub struct AgentError {
    pub agent_id: String,
    pub class: String,
}

fn error_class(v: &Value) -> String {
    let raw = raw_text(v);
    let first = raw.split(['\n', ':', '(']).next().unwrap_or("");
    if first.trim().chars().count() > 64 {
        return "error".to_string();
    }
    let head = upstream_text(&Value::String(first.to_string()));
    let head = head.trim();
    let mut chars = head.chars();
    let valid = head.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        head.to_string()
    } else {
        "error".to_string()
    }
}

fn str_opt(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(str_opt).collect())
        .unwrap_or_default()
}

fn parse_ms(date: &str) -> Option<i64> {
    Timestamp::parse_rfc3339(date).ok().map(|t| t.unix_millis())
}

fn parse_backup(instance: &str, b: &Value) -> Option<HaBackup> {
    let backup_id = str_opt(&b["backup_id"])?;
    let date = str_opt(&b["date"]).unwrap_or_default();
    let created_ms = parse_ms(&date).unwrap_or(0);
    let mut copies: Vec<BackupCopy> = b["agents"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(id, a)| BackupCopy {
                    agent_id: upstream_text(&Value::String(id.clone())),
                    protected: a["protected"].as_bool().unwrap_or(false),
                    size_bytes: a["size"].as_u64().unwrap_or(0),
                    off_box: is_off_box(id),
                })
                .collect()
        })
        .unwrap_or_default();
    copies.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    let full = b["homeassistant_included"].as_bool() == Some(true)
        && b["database_included"].as_bool() == Some(true);
    let size_bytes = copies.iter().map(|c| c.size_bytes).max().unwrap_or(0);
    let name = str_opt(&b["name"]).unwrap_or_default();
    let record = BackupRecord {
        id: backup_id.clone(),
        kind: RECORD_KIND.to_string(),
        instance: instance.to_string(),
        created_ms,
        path: String::new(),
        size_bytes,
        file_count: 0,
        checksum: None,
        note: Some(format!(
            "{name} [{}]",
            copies
                .iter()
                .map(|c| c.agent_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        system: String::new(),
        writer: None,
    };
    Some(HaBackup {
        backup_id,
        name,
        date,
        created_ms,
        backup_type: if full {
            BackupType::Full
        } else {
            BackupType::Partial
        },
        protected: copies.iter().any(|c| c.protected),
        size_bytes,
        homeassistant_version: str_opt(&b["homeassistant_version"]),
        automatic: b["with_automatic_settings"].as_bool().unwrap_or(false),
        failed_agent_ids: str_list(&b["failed_agent_ids"]),
        copies,
        record,
    })
}

pub fn parse_list(instance: &str, info: &Value, agents: &Value) -> HaBackupList {
    let mut backups: Vec<HaBackup> = info["backups"]
        .as_array()
        .map(|a| a.iter().filter_map(|b| parse_backup(instance, b)).collect())
        .unwrap_or_default();
    backups.sort_by_key(|b| std::cmp::Reverse(b.created_ms));
    let agent_errors = info["agent_errors"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| AgentError {
                    agent_id: upstream_text(&Value::String(k.clone())),
                    class: error_class(v),
                })
                .collect()
        })
        .unwrap_or_default();
    let agents = agents["agents"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| str_opt(&x["agent_id"]))
                .map(|id| upstream_text(&Value::String(id)))
                .collect()
        })
        .unwrap_or_default();
    HaBackupList {
        backups,
        agents,
        agent_errors,
        last_attempted_automatic_backup: str_opt(&info["last_attempted_automatic_backup"]),
        last_completed_automatic_backup: str_opt(&info["last_completed_automatic_backup"]),
        warnings: Vec::new(),
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Degraded,
    Stale,
    Missing,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    NoBackups,
    NewestStale,
    NoOffBoxCopy,
    OffBoxStale,
    FutureDated,
    NewestFailedOnAgents,
    AutomaticAttemptIncomplete,
    NoOffBoxAgentEnrolled,
    SingleOffBoxAgentEnrolled,
    AgentErrors,
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
pub struct HaBackupCheck {
    pub status: CheckStatus,
    pub backup_count: usize,
    pub newest_age_hours: Option<i64>,
    pub newest_off_box_age_hours: Option<i64>,
    pub max_age_hours: i64,
    pub last_attempted_automatic_backup_ms: Option<i64>,
    pub last_completed_automatic_backup_ms: Option<i64>,
    pub enrolled_agent_ids: Vec<String>,
    pub failed_agent_ids: Vec<String>,
    pub error_agent_ids: Vec<String>,
    pub checksum_verified: bool,
    pub problems: Vec<Problem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn check(
    list: &HaBackupList,
    config: &Value,
    max_age_hours: i64,
    now_ms: i64,
) -> HaBackupCheck {
    let hours = |ms: i64| (now_ms - ms) / 3_600_000;
    let newest = list.backups.first();
    let newest_off_box = list
        .backups
        .iter()
        .find(|b| b.copies.iter().any(|c| c.off_box));
    let enrolled: Vec<String> = str_list(&config["config"]["create_backup"]["agent_ids"])
        .iter()
        .map(|a| upstream_text(&Value::String(a.clone())))
        .collect();
    let mut problems = Vec::new();

    let newest_age = newest.map(|b| hours(b.created_ms));
    let off_box_age = newest_off_box.map(|b| hours(b.created_ms));
    let mut status = match newest_age {
        None => {
            problems.push(Problem::NoBackups);
            CheckStatus::Missing
        }
        Some(age) if age > max_age_hours => {
            problems.push(Problem::NewestStale);
            CheckStatus::Stale
        }
        Some(_) => CheckStatus::Ok,
    };
    let mut degrade = |p: Problem, problems: &mut Vec<Problem>| {
        problems.push(p);
        if status == CheckStatus::Ok {
            status = CheckStatus::Degraded;
        }
    };

    if list
        .backups
        .iter()
        .any(|b| b.created_ms > now_ms + FUTURE_SKEW_MS)
    {
        degrade(Problem::FutureDated, &mut problems);
    }
    match off_box_age {
        None if newest.is_some() => degrade(Problem::NoOffBoxCopy, &mut problems),
        Some(age) if age > max_age_hours => degrade(Problem::OffBoxStale, &mut problems),
        _ => {}
    }
    let failed_agent_ids = newest
        .map(|b| {
            b.failed_agent_ids
                .iter()
                .map(|a| upstream_text(&Value::String(a.clone())))
                .collect()
        })
        .unwrap_or_default();
    if newest.is_some_and(|b| !b.failed_agent_ids.is_empty()) {
        degrade(Problem::NewestFailedOnAgents, &mut problems);
    }
    let attempted = list
        .last_attempted_automatic_backup
        .as_deref()
        .and_then(parse_ms);
    let completed = list
        .last_completed_automatic_backup
        .as_deref()
        .and_then(parse_ms);
    if let Some(a) = attempted {
        if completed.is_none_or(|c| c < a) {
            degrade(Problem::AutomaticAttemptIncomplete, &mut problems);
        }
    }
    match enrolled.iter().filter(|a| is_off_box(a)).count() {
        0 => degrade(Problem::NoOffBoxAgentEnrolled, &mut problems),
        1 => degrade(Problem::SingleOffBoxAgentEnrolled, &mut problems),
        _ => {}
    }
    if !list.agent_errors.is_empty() {
        degrade(Problem::AgentErrors, &mut problems);
    }

    HaBackupCheck {
        status,
        backup_count: list.backups.len(),
        newest_age_hours: newest_age,
        newest_off_box_age_hours: off_box_age,
        max_age_hours,
        last_attempted_automatic_backup_ms: attempted,
        last_completed_automatic_backup_ms: completed,
        enrolled_agent_ids: enrolled,
        failed_agent_ids,
        error_agent_ids: list
            .agent_errors
            .iter()
            .map(|e| e.agent_id.clone())
            .collect(),
        checksum_verified: false,
        problems,
        warnings: Vec::new(),
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "snake_case")]
pub enum AutomaticResult {
    Never,
    Succeeded,
    Partial,
    Failed,
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
pub struct FailedUpload {
    pub backup_id: String,
    pub date: String,
    pub automatic: bool,
    pub failed_agent_ids: Vec<String>,
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
pub struct HaBackupStatus {
    pub agents: Vec<String>,
    pub enrolled_agent_ids: Vec<String>,
    pub last_attempted_automatic_backup: Option<String>,
    pub last_completed_automatic_backup: Option<String>,
    pub last_automatic_result: AutomaticResult,
    pub failed_uploads: Vec<FailedUpload>,
    pub agent_errors: Vec<AgentError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn status(list: &HaBackupList, config: &Value) -> HaBackupStatus {
    let attempted = list
        .last_attempted_automatic_backup
        .as_deref()
        .and_then(parse_ms);
    let completed = list
        .last_completed_automatic_backup
        .as_deref()
        .and_then(parse_ms);
    let newest_auto_failed = list
        .backups
        .iter()
        .find(|b| b.automatic)
        .is_some_and(|b| !b.failed_agent_ids.is_empty());
    let last_automatic_result = match (attempted, completed) {
        (None, None) => AutomaticResult::Never,
        (Some(a), c) if c.is_none_or(|c| c < a) => AutomaticResult::Failed,
        _ if newest_auto_failed => AutomaticResult::Partial,
        _ => AutomaticResult::Succeeded,
    };
    let failed_uploads = list
        .backups
        .iter()
        .filter(|b| !b.failed_agent_ids.is_empty())
        .map(|b| FailedUpload {
            backup_id: upstream_text(&Value::String(b.backup_id.clone())),
            date: b.date.clone(),
            automatic: b.automatic,
            failed_agent_ids: b
                .failed_agent_ids
                .iter()
                .map(|a| upstream_text(&Value::String(a.clone())))
                .collect(),
        })
        .collect();
    HaBackupStatus {
        agents: list.agents.clone(),
        enrolled_agent_ids: str_list(&config["config"]["create_backup"]["agent_ids"])
            .iter()
            .map(|a| upstream_text(&Value::String(a.clone())))
            .collect(),
        last_attempted_automatic_backup: list.last_attempted_automatic_backup.clone(),
        last_completed_automatic_backup: list.last_completed_automatic_backup.clone(),
        last_automatic_result,
        failed_uploads,
        agent_errors: list.agent_errors.clone(),
        warnings: Vec::new(),
    }
}

pub const MAX_AGE_HOURS_RANGE: std::ops::RangeInclusive<i64> = 1..=8760;

pub fn validate_max_age_hours(h: i64) -> Result<()> {
    if !MAX_AGE_HOURS_RANGE.contains(&h) {
        bail!("max_age_hours must be between 1 and 8760, got {h}");
    }
    Ok(())
}

impl Client {
    pub async fn backup_list(&self, instance: &str) -> Result<HaBackupList> {
        let r = self
            .ws_commands(&["backup/info", "backup/agents/info"])
            .await?;
        let mut list = parse_list(instance, &r[0], &r[1]);
        list.warnings = self.warnings();
        Ok(list)
    }

    pub async fn backup_status(&self, instance: &str) -> Result<HaBackupStatus> {
        let r = self
            .ws_commands(&["backup/info", "backup/agents/info", "backup/config/info"])
            .await?;
        let mut out = status(&parse_list(instance, &r[0], &r[1]), &r[2]);
        out.warnings = self.warnings();
        Ok(out)
    }

    pub async fn backup_check(&self, instance: &str, max_age_hours: i64) -> Result<HaBackupCheck> {
        validate_max_age_hours(max_age_hours)?;
        let r = self
            .ws_commands(&["backup/info", "backup/agents/info", "backup/config/info"])
            .await?;
        let list = parse_list(instance, &r[0], &r[1]);
        let mut out = check(
            &list,
            &r[2],
            max_age_hours,
            plugin_toolkit::time::now().unix_millis(),
        );
        out.warnings = self.warnings();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use plugin_toolkit::capsink::{with_cap_sink, CapSink};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    type Calls = Rc<RefCell<Vec<(String, Value)>>>;

    fn sink(replies: Vec<Value>) -> (CapSink, Calls) {
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&calls);
        let mut replies: VecDeque<_> = replies.into();
        let sink: CapSink = Box::new(move |cap: &str, op: &str| {
            seen.borrow_mut().push((
                cap.to_string(),
                plugin_toolkit::serde_json::from_str(op).unwrap(),
            ));
            Ok(replies
                .pop_front()
                .expect("unexpected cap call")
                .to_string())
        });
        (sink, calls)
    }

    fn text(v: Value) -> Value {
        json!({"t": "text", "text": v.to_string()})
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn info() -> Value {
        json!({
            "backups": [
                {
                    "backup_id": "old1", "name": "Old", "date": "2026-10-01T04:00:00+00:00",
                    "homeassistant_included": true, "database_included": false,
                    "agents": {"hassio.local": {"protected": false, "size": 10}},
                    "failed_agent_ids": [], "with_automatic_settings": false
                },
                {
                    "backup_id": "new1", "name": "Automatic", "date": "2026-10-16T04:00:00.123456+00:00",
                    "homeassistant_included": true, "database_included": true,
                    "homeassistant_version": "2026.10.1",
                    "agents": {
                        "hassio.willow": {"protected": true, "size": 77864960},
                        "hassio.local": {"protected": true, "size": 77864960}
                    },
                    "failed_agent_ids": [], "with_automatic_settings": true
                }
            ],
            "agent_errors": {},
            "last_attempted_automatic_backup": "2026-10-16T04:00:00+00:00",
            "last_completed_automatic_backup": "2026-10-16T04:00:05+00:00"
        })
    }

    fn agents() -> Value {
        json!({"agents": [{"agent_id": "hassio.local", "name": "local"}, {"agent_id": "hassio.willow", "name": "willow"}]})
    }

    fn config(ids: &[&str]) -> Value {
        json!({"config": {"create_backup": {"agent_ids": ids, "password": "hunter2"}}})
    }

    fn now() -> i64 {
        parse_ms("2026-10-16T10:00:00+00:00").unwrap()
    }

    #[test]
    fn ws_url_maps_scheme_and_path() {
        assert_eq!(ws_url("http://ha:8123/"), "ws://ha:8123/api/websocket");
        assert_eq!(
            ws_url("https://ha.example"),
            "wss://ha.example/api/websocket"
        );
    }

    #[test]
    fn lists_newest_first_with_derived_fields() {
        let l = parse_list("frigg", &info(), &agents());
        assert_eq!(l.backups.len(), 2);
        let b = &l.backups[0];
        assert_eq!(b.backup_id, "new1");
        assert_eq!(b.backup_type, BackupType::Full);
        assert!(b.protected);
        assert!(b.automatic);
        assert_eq!(b.size_bytes, 77864960);
        assert_eq!(b.copies[0].agent_id, "hassio.local");
        assert!(!b.copies[0].off_box);
        assert!(b.copies[1].off_box);
        assert_eq!(b.record.kind, RECORD_KIND);
        assert_eq!(b.record.instance, "frigg");
        assert_eq!(b.record.created_ms, b.created_ms);
        assert!(b.record.checksum.is_none());
        assert_eq!(l.backups[1].backup_type, BackupType::Partial);
        assert_eq!(l.agents, ["hassio.local", "hassio.willow"]);
    }

    #[test]
    fn single_off_box_agent_is_degraded() {
        let l = parse_list("i", &info(), &agents());
        let c = check(&l, &config(&["hassio.local", "hassio.willow"]), 48, now());
        assert_eq!(c.status, CheckStatus::Degraded);
        assert_eq!(c.newest_age_hours, Some(5));
        assert!(!c.checksum_verified);
        assert!(c.problems.contains(&Problem::SingleOffBoxAgentEnrolled));
    }

    #[test]
    fn two_off_box_agents_and_fresh_is_ok() {
        let l = parse_list("i", &info(), &agents());
        let c = check(&l, &config(&["hassio.willow", "hassio.maple"]), 48, now());
        assert_eq!(c.status, CheckStatus::Ok, "{:?}", c.problems);
    }

    #[test]
    fn stale_and_missing() {
        let l = parse_list("i", &info(), &agents());
        let c = check(&l, &config(&["hassio.willow", "hassio.maple"]), 2, now());
        assert_eq!(c.status, CheckStatus::Stale);
        let empty = parse_list("i", &json!({"backups": []}), &agents());
        let c = check(
            &empty,
            &config(&["hassio.willow", "hassio.maple"]),
            48,
            now(),
        );
        assert_eq!(c.status, CheckStatus::Missing);
        assert_eq!(c.backup_count, 0);
    }

    #[test]
    fn attempted_after_completed_and_failed_agents_degrade() {
        let mut i = info();
        i["last_completed_automatic_backup"] = json!("2026-10-02T04:00:00+00:00");
        i["backups"][1]["failed_agent_ids"] = json!(["hassio.willow"]);
        let l = parse_list("i", &i, &agents());
        let c = check(&l, &config(&["hassio.willow", "hassio.maple"]), 48, now());
        assert_eq!(c.status, CheckStatus::Degraded);
        assert!(c.problems.contains(&Problem::AutomaticAttemptIncomplete));
        assert!(c.problems.contains(&Problem::NewestFailedOnAgents));
        assert_eq!(c.failed_agent_ids, ["hassio.willow"]);
    }

    #[test]
    fn check_never_echoes_the_backup_password() {
        let l = parse_list("i", &info(), &agents());
        let c = check(&l, &config(&["hassio.willow"]), 48, now());
        let out = plugin_toolkit::serde_json::to_string(&c).unwrap();
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn backup_list_authenticates_and_runs_commands_over_ws() {
        let (s, calls) = sink(vec![
            json!({"t": "open", "session": 3}),
            text(json!({"type": "auth_required"})),
            Value::Null,
            text(json!({"type": "auth_ok"})),
            Value::Null,
            text(json!({"id": 1, "type": "result", "success": true, "result": info()})),
            Value::Null,
            text(json!({"type": "event", "event": {}})),
            text(json!({"id": 2, "type": "result", "success": true, "result": agents()})),
            Value::Null,
        ]);
        let l = with_cap_sink(s, || {
            block_on(Client::new(Config::new("https://ha.test:8123", "tok")).backup_list("frigg"))
        })
        .unwrap();
        assert_eq!(l.backups[0].backup_id, "new1");
        let calls = calls.borrow();
        assert_eq!(calls[0].0, "ws.open");
        assert_eq!(calls[0].1["url"], "wss://ha.test:8123/api/websocket");
        let sent: Vec<Value> = calls
            .iter()
            .filter(|(c, _)| c == "ws.send")
            .map(|(_, op)| {
                plugin_toolkit::serde_json::from_str(op["text"].as_str().unwrap()).unwrap()
            })
            .collect();
        assert_eq!(sent[0], json!({"type": "auth", "access_token": "tok"}));
        assert_eq!(sent[1], json!({"id": 1, "type": "backup/info"}));
        assert_eq!(sent[2], json!({"id": 2, "type": "backup/agents/info"}));
        assert_eq!(calls.last().unwrap().0, "ws.close");
    }

    #[test]
    fn status_reports_failed_uploads_and_result() {
        let mut i = info();
        i["backups"][1]["failed_agent_ids"] = json!(["hassio.maple"]);
        let s = status(
            &parse_list("i", &i, &agents()),
            &config(&["hassio.local", "hassio.maple"]),
        );
        assert_eq!(s.last_automatic_result, AutomaticResult::Partial);
        assert_eq!(s.failed_uploads.len(), 1);
        assert_eq!(s.failed_uploads[0].backup_id, "new1");
        assert_eq!(s.failed_uploads[0].failed_agent_ids, ["hassio.maple"]);
        assert!(s.failed_uploads[0].automatic);
        assert_eq!(s.enrolled_agent_ids, ["hassio.local", "hassio.maple"]);
        let out = plugin_toolkit::serde_json::to_string(&s).unwrap();
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn status_automatic_result_states() {
        let mut i = info();
        assert_eq!(
            status(&parse_list("i", &i, &agents()), &config(&[])).last_automatic_result,
            AutomaticResult::Succeeded
        );
        i["last_completed_automatic_backup"] = json!("2026-10-01T04:00:00+00:00");
        assert_eq!(
            status(&parse_list("i", &i, &agents()), &config(&[])).last_automatic_result,
            AutomaticResult::Failed
        );
        i["last_attempted_automatic_backup"] = Value::Null;
        i["last_completed_automatic_backup"] = Value::Null;
        assert_eq!(
            status(&parse_list("i", &i, &agents()), &config(&[])).last_automatic_result,
            AutomaticResult::Never
        );
    }

    #[test]
    fn rejected_token_errors_without_echoing_it() {
        let (s, _) = sink(vec![
            json!({"t": "open", "session": 1}),
            text(json!({"type": "auth_required"})),
            Value::Null,
            text(json!({"type": "auth_invalid", "message": "Invalid access token"})),
            Value::Null,
        ]);
        let err = with_cap_sink(s, || {
            block_on(Client::new(Config::new("http://ha", "secret-tok")).backup_list("i"))
        })
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rejected the token"), "{msg}");
        assert!(!msg.contains("secret-tok"));
    }

    #[test]
    fn failed_command_surfaces_ha_error() {
        let (s, _) = sink(vec![
            json!({"t": "open", "session": 1}),
            text(json!({"type": "auth_required"})),
            Value::Null,
            text(json!({"type": "auth_ok"})),
            Value::Null,
            text(
                json!({"id": 1, "type": "result", "success": false, "error": {"code": "unauthorized", "message": "Unauthorized"}}),
            ),
            Value::Null,
        ]);
        let err = with_cap_sink(s, || {
            block_on(Client::new(Config::new("http://ha", "t")).backup_list("i"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("backup/info failed: Unauthorized"));
    }

    #[test]
    fn future_dated_backup_degrades() {
        let mut i = info();
        i["backups"][0]["date"] = json!("2027-01-01T00:00:00+00:00");
        let l = parse_list("i", &i, &agents());
        let c = check(&l, &config(&["hassio.willow", "hassio.maple"]), 48, now());
        assert_eq!(c.status, CheckStatus::Degraded);
        assert!(c.problems.contains(&Problem::FutureDated));
    }

    #[test]
    fn max_age_hours_bounds() {
        for bad in [0, -1, 8761] {
            assert!(validate_max_age_hours(bad).is_err(), "{bad}");
        }
        for ok in [1, 48, 8760] {
            assert!(validate_max_age_hours(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn agent_errors_keep_id_and_short_scrubbed_class() {
        let mut i = info();
        let long = format!(
            "BackupAgentUnreachableError: password=hunter2 {}",
            "x".repeat(200)
        );
        i["agent_errors"] = json!({"hassio.willow": long});
        let l = parse_list("i", &i, &agents());
        assert_eq!(l.agent_errors.len(), 1);
        assert_eq!(l.agent_errors[0].agent_id, "hassio.willow");
        assert_eq!(l.agent_errors[0].class, "BackupAgentUnreachableError");
        let c = check(&l, &config(&["hassio.willow", "hassio.maple"]), 48, now());
        assert!(c.problems.contains(&Problem::AgentErrors));
        assert_eq!(c.error_agent_ids, ["hassio.willow"]);
    }

    #[test]
    fn upstream_text_is_capped_and_scrubbed() {
        let t = upstream_text(&json!(format!(
            "Authorization: Bearer abc123secret {}",
            "y".repeat(500)
        )));
        assert!(t.chars().count() <= MAX_UPSTREAM_TEXT);
        assert!(!t.contains("abc123secret"), "{t}");
    }

    #[test]
    fn check_output_carries_no_names_or_error_text() {
        let mut i = info();
        i["agent_errors"] = json!({"hassio.willow": "Mount failed: server 10.0.0.9 down"});
        let l = parse_list("i", &i, &agents());
        let c = check(&l, &config(&["hassio.willow"]), 48, now());
        let out = plugin_toolkit::serde_json::to_string(&c).unwrap();
        for leaked in [
            "\"Automatic\"",
            "\"Old\"",
            "Mount failed",
            "10.0.0.9",
            "2026.10.1",
        ] {
            assert!(!out.contains(leaked), "{leaked} in {out}");
        }
    }

    #[test]
    fn unrelated_message_flood_is_capped() {
        let mut replies = vec![
            json!({"t": "open", "session": 1}),
            text(json!({"type": "auth_required"})),
            Value::Null,
            text(json!({"type": "auth_ok"})),
            Value::Null,
        ];
        replies.extend((0..=MAX_SKIPPED_MESSAGES).map(|_| text(json!({"type": "event"}))));
        replies.push(Value::Null);
        let (s, _) = sink(replies);
        let err = with_cap_sink(s, || {
            block_on(Client::new(Config::new("http://ha", "t")).backup_list("i"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("unrelated messages"), "{err}");
    }

    #[test]
    fn total_deadline_bails() {
        let (s, _) = sink(vec![
            json!({"t": "open", "session": 1}),
            text(json!({"type": "auth_required"})),
            Value::Null,
            text(json!({"type": "auth_ok"})),
            Value::Null,
            Value::Null,
        ]);
        let err = with_cap_sink(s, || {
            block_on(
                Client::new(Config::new("http://ha", "t"))
                    .ws_commands_within(&["backup/info"], Duration::ZERO),
            )
        })
        .unwrap_err();
        assert!(err.to_string().contains("deadline"), "{err}");
    }

    #[test]
    fn error_class_rejects_paths_and_free_text() {
        assert_eq!(
            error_class(&json!("Mount 10.0.0.9:/mnt/user/backups/ha is down")),
            "error"
        );
        assert_eq!(
            error_class(&json!("could not reach //nas/share/ha")),
            "error"
        );
        assert_eq!(
            error_class(&json!("BackupAgentError: boom")),
            "BackupAgentError"
        );
        assert_eq!(error_class(&json!("1abc: x")), "error");
    }

    #[test]
    fn upstream_text_strips_control_and_bidi_chars() {
        let t = upstream_text(&json!("ok\u{202E}gnp.exe\u{2066}\n\u{1b}[31mred"));
        assert_eq!(t, "okgnp.exered");
        let t = upstream_text(&json!("a\u{200B}b\u{061C}c\u{FEFF}d\u{2028}e\u{1b}[1;32mf"));
        assert_eq!(t, "abcdef");
    }

    #[test]
    fn error_class_limits_and_unicode() {
        let ok = format!("A{}", "b".repeat(63));
        assert_eq!(error_class(&json!(format!("{ok}: x"))), ok);
        assert_eq!(
            error_class(&json!(format!("A{}: x", "b".repeat(64)))),
            "error"
        );
        assert_eq!(error_class(&json!("Fehlerä: x")), "error");
        assert_eq!(
            error_class(&json!("BackupError\nsecond: line")),
            "BackupError"
        );
    }

    #[test]
    fn silent_server_times_out_at_the_deadline() {
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&calls);
        let mut replies: VecDeque<Value> = vec![
            json!({"t": "open", "session": 1}),
            text(json!({"type": "auth_required"})),
        ]
        .into();
        let s: CapSink = Box::new(move |cap: &str, op: &str| {
            seen.borrow_mut().push((
                cap.to_string(),
                plugin_toolkit::serde_json::from_str(op).unwrap(),
            ));
            if cap == "ws.recv" && replies.is_empty() {
                return Err("timed out".to_string());
            }
            Ok(replies.pop_front().unwrap_or(Value::Null).to_string())
        });
        let budget = Duration::from_millis(1500);
        let err = with_cap_sink(s, || {
            block_on(
                Client::new(Config::new("http://ha", "t"))
                    .ws_commands_within(&["backup/info"], budget),
            )
        })
        .unwrap_err();
        assert!(err.to_string().contains("deadline"), "{err}");
        let calls = calls.borrow();
        let waits: Vec<u64> = calls
            .iter()
            .filter(|(c, _)| c == "ws.recv")
            .map(|(_, op)| op["timeout_ms"].as_u64().expect("bounded recv"))
            .collect();
        assert_eq!(waits.len(), 2);
        assert!(waits.iter().all(|w| *w <= 1500), "{waits:?}");
    }
}
