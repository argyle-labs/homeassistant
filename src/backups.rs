#![allow(clippy::disallowed_types)]

use plugin_toolkit::abi::WsOpen;
use plugin_toolkit::contract::backup::BackupRecord;
use plugin_toolkit::delegated_ws::WsClient;
use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Value};
use plugin_toolkit::time::Timestamp;

use crate::Client;

pub const RECORD_KIND: &str = "home-assistant";

const ON_BOX_AGENTS: &[&str] = &["hassio.local", "backup.local"];

const MAX_MESSAGE_BYTES: u64 = 16 << 20;

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

async fn recv_json(ws: &WsClient) -> Result<Value> {
    let text = ws
        .recv()
        .await?
        .context("home assistant closed the websocket")?;
    Ok(plugin_toolkit::serde_json::from_str(&text)?)
}

impl Client {
    pub async fn ws_commands(&self, types: &[&str]) -> Result<Vec<Value>> {
        let ws = WsClient::connect(WsOpen {
            url: ws_url(&self.cfg.base_url),
            headers: Vec::new(),
            insecure: false,
            timeout_ms: None,
            max_message_bytes: Some(MAX_MESSAGE_BYTES),
        })
        .await?;
        let hello = recv_json(&ws).await?;
        if hello["type"] != "auth_required" {
            bail!("unexpected websocket greeting: {}", hello["type"]);
        }
        ws.send(json!({"type": "auth", "access_token": self.cfg.token}).to_string())
            .await?;
        let auth = recv_json(&ws).await?;
        if auth["type"] != "auth_ok" {
            bail!("home assistant rejected the token ({})", auth["type"]);
        }
        let mut out = Vec::with_capacity(types.len());
        for (i, ty) in types.iter().enumerate() {
            let id = i as u64 + 1;
            ws.send(json!({"id": id, "type": ty}).to_string()).await?;
            let reply = loop {
                let msg = recv_json(&ws).await?;
                if msg["id"] == id && msg["type"] == "result" {
                    break msg;
                }
            };
            if reply["success"] != true {
                bail!(
                    "{ty} failed: {}",
                    reply["error"]["message"]
                        .as_str()
                        .unwrap_or("unknown error")
                );
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
    pub agent_errors: Vec<String>,
    pub last_attempted_automatic_backup: Option<String>,
    pub last_completed_automatic_backup: Option<String>,
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
                    agent_id: id.clone(),
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
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or("error")))
                .collect()
        })
        .unwrap_or_default();
    let agents = agents["agents"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| str_opt(&x["agent_id"])).collect())
        .unwrap_or_default();
    HaBackupList {
        backups,
        agents,
        agent_errors,
        last_attempted_automatic_backup: str_opt(&info["last_attempted_automatic_backup"]),
        last_completed_automatic_backup: str_opt(&info["last_completed_automatic_backup"]),
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
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct HaBackupCheck {
    pub status: CheckStatus,
    pub newest_backup_id: Option<String>,
    pub newest_age_hours: Option<i64>,
    pub newest_off_box_age_hours: Option<i64>,
    pub max_age_hours: i64,
    pub last_attempted_automatic_backup: Option<String>,
    pub last_completed_automatic_backup: Option<String>,
    pub enrolled_agent_ids: Vec<String>,
    pub checksum_verified: bool,
    pub problems: Vec<String>,
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
    let enrolled = str_list(&config["config"]["create_backup"]["agent_ids"]);
    let mut problems = Vec::new();

    let newest_age = newest.map(|b| hours(b.created_ms));
    let off_box_age = newest_off_box.map(|b| hours(b.created_ms));
    let mut status = match newest_age {
        None => {
            problems.push("no backups found".to_string());
            CheckStatus::Missing
        }
        Some(age) if age > max_age_hours => {
            problems.push(format!(
                "newest backup is {age}h old (limit {max_age_hours}h)"
            ));
            CheckStatus::Stale
        }
        Some(_) => CheckStatus::Ok,
    };
    let mut degrade = |msg: String, problems: &mut Vec<String>| {
        problems.push(msg);
        if status == CheckStatus::Ok {
            status = CheckStatus::Degraded;
        }
    };

    match off_box_age {
        None if newest.is_some() => degrade("no backup has an off-box copy".into(), &mut problems),
        Some(age) if age > max_age_hours => degrade(
            format!("newest off-box backup is {age}h old (limit {max_age_hours}h)"),
            &mut problems,
        ),
        _ => {}
    }
    if let Some(b) = newest.filter(|b| !b.failed_agent_ids.is_empty()) {
        degrade(
            format!("newest backup failed on {}", b.failed_agent_ids.join(", ")),
            &mut problems,
        );
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
            degrade(
                "last automatic backup attempt did not complete".into(),
                &mut problems,
            );
        }
    }
    let off_box_enrolled = enrolled.iter().filter(|a| is_off_box(a)).count();
    match off_box_enrolled {
        0 => degrade(
            "no off-box agent enrolled in automatic backups".into(),
            &mut problems,
        ),
        1 => degrade(
            "only one off-box agent enrolled in automatic backups".into(),
            &mut problems,
        ),
        _ => {}
    }
    for e in &list.agent_errors {
        degrade(format!("agent error: {e}"), &mut problems);
    }

    HaBackupCheck {
        status,
        newest_backup_id: newest.map(|b| b.backup_id.clone()),
        newest_age_hours: newest_age,
        newest_off_box_age_hours: off_box_age,
        max_age_hours,
        last_attempted_automatic_backup: list.last_attempted_automatic_backup.clone(),
        last_completed_automatic_backup: list.last_completed_automatic_backup.clone(),
        enrolled_agent_ids: enrolled,
        checksum_verified: false,
        problems,
    }
}

impl Client {
    pub async fn backup_list(&self, instance: &str) -> Result<HaBackupList> {
        let r = self
            .ws_commands(&["backup/info", "backup/agents/info"])
            .await?;
        Ok(parse_list(instance, &r[0], &r[1]))
    }

    pub async fn backup_check(&self, instance: &str, max_age_hours: i64) -> Result<HaBackupCheck> {
        let r = self
            .ws_commands(&["backup/info", "backup/agents/info", "backup/config/info"])
            .await?;
        let list = parse_list(instance, &r[0], &r[1]);
        Ok(check(
            &list,
            &r[2],
            max_age_hours,
            plugin_toolkit::time::now().unix_millis(),
        ))
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
        assert!(c.problems.iter().any(|p| p.contains("only one off-box")));
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
        assert!(c.newest_backup_id.is_none());
    }

    #[test]
    fn attempted_after_completed_and_failed_agents_degrade() {
        let mut i = info();
        i["last_completed_automatic_backup"] = json!("2026-10-02T04:00:00+00:00");
        i["backups"][1]["failed_agent_ids"] = json!(["hassio.willow"]);
        let l = parse_list("i", &i, &agents());
        let c = check(&l, &config(&["hassio.willow", "hassio.maple"]), 48, now());
        assert_eq!(c.status, CheckStatus::Degraded);
        assert!(c.problems.iter().any(|p| p.contains("did not complete")));
        assert!(c
            .problems
            .iter()
            .any(|p| p.contains("failed on hassio.willow")));
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
}
