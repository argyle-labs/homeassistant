#![allow(clippy::disallowed_types)]

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Map, Value};

use crate::backups::upstream_text;
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
pub struct HaMount {
    pub name: String,
    #[serde(rename = "type")]
    pub mount_type: String,
    pub usage: String,
    pub server: String,
    pub share: String,
    pub username: Option<String>,
    pub read_only: bool,
    pub state: Option<String>,
    pub failed: bool,
    pub default_backup_location: bool,
    pub issues: Vec<String>,
    pub suggestions: Vec<String>,
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
pub struct HaMountList {
    pub mounts: Vec<HaMount>,
    pub default_backup_mount: Option<String>,
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
pub struct HaRepairIssue {
    pub domain: String,
    pub issue_id: String,
    pub severity: String,
    pub translation_key: Option<String>,
    pub placeholders: Map<String, Value>,
    pub created: Option<String>,
    pub ignored: bool,
    pub is_fixable: bool,
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
pub struct HaRepairList {
    pub issues: Vec<HaRepairIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MountChange {
    pub server: Option<String>,
    pub share: Option<String>,
    pub username: Option<String>,
    pub password_secret: Option<String>,
}

impl MountChange {
    fn is_empty(&self) -> bool {
        *self == Self::default()
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
pub struct HaMountRepair {
    pub dry_run: bool,
    pub mount: String,
    pub plan: Vec<String>,
    pub before: HaMount,
    pub after: Option<HaMount>,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
pub struct HaMountHealed {
    pub mount: String,
    pub before: HaMount,
    pub after: Option<HaMount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
pub struct HaMountHeal {
    pub dry_run: bool,
    pub plan: Vec<String>,
    pub mounts: Vec<HaMountHealed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn text(v: &Value) -> String {
    upstream_text(v)
}

fn text_opt(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| !s.is_empty())
        .map(|_| upstream_text(v))
}

fn supervisor(endpoint: &str, method: &str, data: Option<Value>) -> Value {
    let mut msg = json!({"type": "supervisor/api", "endpoint": endpoint, "method": method});
    if let Some(d) = data {
        msg["data"] = d;
    }
    msg
}

fn mount_name(name: &str) -> Result<&str> {
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64;
    if !valid {
        bail!("mount name must match [A-Za-z][A-Za-z0-9_]*");
    }
    Ok(name)
}

fn by_type(items: &Value, name: &str) -> Vec<String> {
    items
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i["context"] == "mount" && i["reference"] == name)
                .filter_map(|i| text_opt(&i["type"]))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_mounts(mounts: &Value, resolution: &Value) -> HaMountList {
    let default_backup_mount = text_opt(&mounts["default_backup_mount"]);
    let list = mounts["mounts"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let name = m["name"].as_str()?.to_string();
                    let issues = by_type(&resolution["issues"], &name);
                    let state = text_opt(&m["state"]);
                    let share = if m["share"].is_string() {
                        text(&m["share"])
                    } else {
                        text(&m["path"])
                    };
                    Some(HaMount {
                        mount_type: text(&m["type"]),
                        usage: text(&m["usage"]),
                        server: text(&m["server"]),
                        share,
                        username: text_opt(&m["username"]),
                        read_only: m["read_only"].as_bool().unwrap_or(false),
                        failed: !issues.is_empty()
                            || state.as_deref().is_some_and(|s| s != "active"),
                        state,
                        default_backup_location: default_backup_mount.as_deref()
                            == Some(name.as_str()),
                        suggestions: by_type(&resolution["suggestions"], &name),
                        issues,
                        name: text(&Value::String(name)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    HaMountList {
        mounts: list,
        default_backup_mount,
        warnings: Vec::new(),
    }
}

pub fn parse_repairs(v: &Value) -> HaRepairList {
    let issues = v["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| HaRepairIssue {
                    domain: text(&i["domain"]),
                    issue_id: text(&i["issue_id"]),
                    severity: text(&i["severity"]),
                    translation_key: text_opt(&i["translation_key"]),
                    placeholders: i["translation_placeholders"]
                        .as_object()
                        .map(|m| {
                            m.iter()
                                .map(|(k, v)| {
                                    (text(&Value::String(k.clone())), Value::String(text(v)))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    created: text_opt(&i["created"]),
                    ignored: i["ignored"].as_bool().unwrap_or(false),
                    is_fixable: i["is_fixable"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    HaRepairList {
        issues,
        warnings: Vec::new(),
    }
}

fn update_body(raw: &Value, change: &MountChange, password: Option<String>) -> Result<Value> {
    let ty = raw["type"].as_str().unwrap_or_default();
    let mut body = Map::new();
    for key in ["type", "usage", "server", "read_only", "port", "version"] {
        if !raw[key].is_null() {
            body.insert(key.to_string(), raw[key].clone());
        }
    }
    let share_key = if ty == "nfs" { "path" } else { "share" };
    body.insert(share_key.to_string(), raw[share_key].clone());
    if let Some(s) = &change.server {
        body.insert("server".into(), json!(s));
    }
    if let Some(s) = &change.share {
        body.insert(share_key.into(), json!(s));
    }
    match ty {
        "cifs" => {
            let password = password.context(
                "a cifs mount update needs password_secret: Supervisor never returns the stored password",
            )?;
            let username = change
                .username
                .clone()
                .or_else(|| raw["username"].as_str().map(str::to_string));
            if let Some(u) = username {
                body.insert("username".into(), json!(u));
            }
            body.insert("password".into(), json!(password));
        }
        _ if change.username.is_some() || change.password_secret.is_some() => {
            bail!("username/password apply only to cifs mounts; '{ty}' takes none")
        }
        _ => {}
    }
    Ok(Value::Object(body))
}

fn redact(e: &impl std::fmt::Display, secret: Option<&str>) -> String {
    let text = format!("{e:#}");
    match secret.filter(|p| !p.is_empty()) {
        Some(p) => text.replace(p, "<redacted>"),
        None => text,
    }
}

fn needs_heal(m: &HaMount) -> bool {
    m.failed || m.state.as_deref() != Some("active")
}

fn find_mount(list: HaMountList, name: &str) -> Option<HaMount> {
    list.mounts.into_iter().find(|m| m.name == name)
}

fn find_raw<'a>(mounts: &'a Value, name: &str) -> Result<&'a Value> {
    mounts["mounts"]
        .as_array()
        .and_then(|a| a.iter().find(|m| m["name"] == name))
        .with_context(|| format!("no supervisor mount named '{name}'"))
}

fn plan(name: &str, change: &MountChange) -> Vec<String> {
    let mut steps = Vec::new();
    if !change.is_empty() {
        let mut fields = Vec::new();
        if let Some(s) = &change.server {
            fields.push(format!("server={s}"));
        }
        if let Some(s) = &change.share {
            fields.push(format!("share={s}"));
        }
        if let Some(u) = &change.username {
            fields.push(format!("username={u}"));
        }
        if let Some(r) = &change.password_secret {
            fields.push(format!("password=<secret {r}>"));
        }
        steps.push(format!("PUT /mounts/{name} ({})", fields.join(", ")));
    }
    steps.push(format!("POST /mounts/{name}/reload"));
    steps
}

impl Client {
    async fn mounts_raw(&self) -> Result<(Value, Value)> {
        let mut r = self
            .ws_requests(&[
                supervisor("/mounts", "get", None),
                supervisor("/resolution/info", "get", None),
            ])
            .await?;
        let resolution = r.pop().unwrap_or_default();
        let mounts = r.pop().unwrap_or_default();
        Ok((mounts, resolution))
    }

    pub async fn mount_list(&self) -> Result<HaMountList> {
        let (mounts, resolution) = self.mounts_raw().await?;
        let mut out = parse_mounts(&mounts, &resolution);
        out.warnings = self.warnings();
        Ok(out)
    }

    pub async fn repairs_list(&self) -> Result<HaRepairList> {
        let r = self.ws_commands(&["repairs/list_issues"]).await?;
        let mut out = parse_repairs(&r[0]);
        out.warnings = self.warnings();
        Ok(out)
    }

    pub async fn mount_repair(
        &self,
        name: &str,
        change: &MountChange,
        execute: bool,
        resolve: impl Fn(&str) -> Result<plugin_toolkit::secrets::Secret<String>>,
    ) -> Result<HaMountRepair> {
        let name = mount_name(name)?;
        let (mounts, resolution) = self.mounts_raw().await?;
        let raw = find_raw(&mounts, name)?.clone();
        let before = parse_mounts(&mounts, &resolution)
            .mounts
            .into_iter()
            .find(|m| m.name == name)
            .with_context(|| format!("no supervisor mount named '{name}'"))?;
        if raw["type"] == "cifs" && !change.is_empty() && change.password_secret.is_none() {
            bail!("a cifs mount update needs password_secret: Supervisor never returns the stored password");
        }
        if !change.is_empty() {
            update_body(&raw, change, Some(String::new()))?;
        }
        let steps = plan(name, change);
        if !execute {
            return Ok(HaMountRepair {
                dry_run: true,
                mount: name.to_string(),
                plan: steps,
                before,
                after: None,
                applied: false,
                reload_error: None,
                warnings: self.warnings(),
            });
        }
        let mut applied = false;
        let mut password = None;
        if !change.is_empty() {
            password = change
                .password_secret
                .as_deref()
                .map(&resolve)
                .transpose()?;
            let put = supervisor(
                &format!("/mounts/{name}"),
                "put",
                Some(update_body(
                    &raw,
                    change,
                    password.as_ref().map(|p| p.expose_secret().clone()),
                )?),
            );
            self.ws_requests(&[put]).await.map_err(|e| {
                anyhow!(redact(
                    &e,
                    password.as_ref().map(|p| p.expose_secret().as_str())
                ))
            })?;
            applied = true;
        }
        let msgs = [
            supervisor(&format!("/mounts/{name}/reload"), "post", None),
            supervisor("/mounts", "get", None),
            supervisor("/resolution/info", "get", None),
        ];
        let (after, reload_error) = match self.ws_requests(&msgs).await {
            Ok(r) => (find_mount(parse_mounts(&r[1], &r[2]), name), None),
            Err(e) if applied => {
                let after = self
                    .mounts_raw()
                    .await
                    .ok()
                    .and_then(|(m, r)| find_mount(parse_mounts(&m, &r), name));
                (
                    after,
                    Some(redact(
                        &e,
                        password.as_ref().map(|p| p.expose_secret().as_str()),
                    )),
                )
            }
            Err(e) => return Err(e),
        };
        Ok(HaMountRepair {
            dry_run: false,
            mount: name.to_string(),
            plan: steps,
            before,
            after,
            applied,
            reload_error,
            warnings: self.warnings(),
        })
    }

    pub async fn mount_heal(&self, execute: bool) -> Result<HaMountHeal> {
        let (mounts, resolution) = self.mounts_raw().await?;
        let targets: Vec<HaMount> = parse_mounts(&mounts, &resolution)
            .mounts
            .into_iter()
            .filter(needs_heal)
            .collect();
        let plan = targets
            .iter()
            .map(|m| format!("POST /mounts/{}/reload", m.name))
            .collect();
        if !execute || targets.is_empty() {
            return Ok(HaMountHeal {
                dry_run: !execute,
                plan,
                mounts: targets
                    .into_iter()
                    .map(|before| HaMountHealed {
                        mount: before.name.clone(),
                        before,
                        after: None,
                        error: None,
                    })
                    .collect(),
                warnings: self.warnings(),
            });
        }
        let mut errors = Vec::with_capacity(targets.len());
        for m in &targets {
            let reload = supervisor(&format!("/mounts/{}/reload", m.name), "post", None);
            errors.push(
                self.ws_requests(&[reload])
                    .await
                    .err()
                    .map(|e| format!("{e:#}")),
            );
        }
        let now = self.mounts_raw().await.ok();
        let healed = targets
            .into_iter()
            .zip(errors)
            .map(|(before, error)| HaMountHealed {
                mount: before.name.clone(),
                after: now
                    .as_ref()
                    .and_then(|(m, r)| find_mount(parse_mounts(m, r), &before.name)),
                before,
                error,
            })
            .collect();
        Ok(HaMountHeal {
            dry_run: false,
            plan,
            mounts: healed,
            warnings: self.warnings(),
        })
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

    fn frame(v: Value) -> Value {
        json!({"t": "text", "text": v.to_string()})
    }

    fn session(results: Vec<Value>) -> Vec<Value> {
        let mut out = vec![
            json!({"t": "open", "session": 1}),
            frame(json!({"type": "auth_required"})),
            Value::Null,
            frame(json!({"type": "auth_ok"})),
        ];
        for (i, r) in results.into_iter().enumerate() {
            out.push(Value::Null);
            out.push(frame(
                json!({"id": i + 1, "type": "result", "success": true, "result": r}),
            ));
        }
        out.push(Value::Null);
        out
    }

    fn sent(calls: &Calls) -> Vec<Value> {
        calls
            .borrow()
            .iter()
            .filter(|(c, _)| c == "ws.send")
            .map(|(_, op)| {
                plugin_toolkit::serde_json::from_str(op["text"].as_str().unwrap()).unwrap()
            })
            .collect()
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn client() -> Client {
        Client::new(Config::new("https://ha.test:8123", "tok"))
    }

    fn mounts() -> Value {
        json!({
            "default_backup_mount": "maple_backups",
            "mounts": [
                {
                    "name": "maple_backups", "usage": "backup", "type": "nfs",
                    "server": "10.0.0.11", "path": "/mnt/user/backups",
                    "read_only": false, "state": "failed"
                },
                {
                    "name": "willow_media", "usage": "media", "type": "cifs",
                    "server": "10.0.0.12", "share": "media", "version": "auto",
                    "username": "ha", "password": "hunter2",
                    "read_only": true, "state": "active"
                }
            ]
        })
    }

    fn resolution() -> Value {
        json!({
            "issues": [
                {"uuid": "a", "type": "mount_failed", "context": "mount", "reference": "maple_backups"},
                {"uuid": "b", "type": "free_space", "context": "system", "reference": null}
            ],
            "suggestions": [
                {"uuid": "c", "type": "execute_reload", "context": "mount", "reference": "maple_backups", "auto": false}
            ]
        })
    }

    #[test]
    fn parse_mounts_reports_usage_failure_and_no_password() {
        let l = parse_mounts(&mounts(), &resolution());
        let maple = &l.mounts[0];
        assert_eq!(maple.usage, "backup");
        assert_eq!(maple.share, "/mnt/user/backups");
        assert!(maple.failed);
        assert!(maple.default_backup_location);
        assert_eq!(maple.issues, ["mount_failed"]);
        assert_eq!(maple.suggestions, ["execute_reload"]);
        let willow = &l.mounts[1];
        assert_eq!(willow.usage, "media");
        assert!(!willow.failed);
        assert!(willow.issues.is_empty());
        let out = plugin_toolkit::serde_json::to_string(&l).unwrap();
        assert!(!out.contains("hunter2"), "{out}");
    }

    #[test]
    fn mount_list_calls_supervisor_over_ws() {
        let (s, calls) = sink(session(vec![mounts(), resolution()]));
        let l = with_cap_sink(s, || block_on(client().mount_list())).unwrap();
        assert_eq!(l.default_backup_mount.as_deref(), Some("maple_backups"));
        let sent = sent(&calls);
        assert_eq!(
            sent[1],
            json!({"id": 1, "type": "supervisor/api", "endpoint": "/mounts", "method": "get"})
        );
        assert_eq!(
            sent[2],
            json!({"id": 2, "type": "supervisor/api", "endpoint": "/resolution/info", "method": "get"})
        );
    }

    #[test]
    fn repairs_list_parses_and_scrubs() {
        let v = json!({"issues": [{
            "domain": "hassio", "issue_id": "issue_mount_mount_failed", "severity": "error",
            "translation_key": "issue_mount_mount_failed",
            "translation_placeholders": {"reference": "maple_backups"},
            "created": "2026-10-17T00:00:00+00:00", "ignored": false, "is_fixable": true
        }]});
        let (s, calls) = sink(session(vec![v]));
        let l = with_cap_sink(s, || block_on(client().repairs_list())).unwrap();
        assert_eq!(l.issues[0].issue_id, "issue_mount_mount_failed");
        assert_eq!(l.issues[0].placeholders["reference"], "maple_backups");
        assert!(l.issues[0].is_fixable);
        assert_eq!(
            sent(&calls)[1],
            json!({"id": 1, "type": "repairs/list_issues"})
        );
    }

    fn no_secret(_: &str) -> Result<plugin_toolkit::secrets::Secret<String>> {
        panic!("secret resolved on a dry run")
    }

    #[test]
    fn repair_defaults_to_dry_run_and_never_writes() {
        let (s, calls) = sink(session(vec![mounts(), resolution()]));
        let change = MountChange {
            server: Some("10.0.0.13".into()),
            ..Default::default()
        };
        let r = with_cap_sink(s, || {
            block_on(client().mount_repair("maple_backups", &change, false, no_secret))
        })
        .unwrap();
        assert!(r.dry_run);
        assert_eq!(
            r.plan,
            [
                "PUT /mounts/maple_backups (server=10.0.0.13)",
                "POST /mounts/maple_backups/reload"
            ]
        );
        assert!(sent(&calls)
            .iter()
            .all(|m| m["method"].is_null() || m["method"] == "get"));
    }

    #[test]
    fn repair_reload_executes() {
        let mut after = mounts();
        after["mounts"][0]["state"] = json!("active");
        let mut replies = session(vec![mounts(), resolution()]);
        replies.extend(session(vec![
            Value::Null,
            after,
            json!({"issues": [], "suggestions": []}),
        ]));
        let (s, calls) = sink(replies);
        let r = with_cap_sink(s, || {
            block_on(client().mount_repair(
                "maple_backups",
                &MountChange::default(),
                true,
                no_secret,
            ))
        })
        .unwrap();
        assert!(!r.dry_run);
        assert!(r.before.failed);
        assert!(!r.after.unwrap().failed);
        assert_eq!(
            sent(&calls)[4],
            json!({"id": 1, "type": "supervisor/api", "endpoint": "/mounts/maple_backups/reload", "method": "post"})
        );
    }

    #[test]
    fn cifs_update_resolves_password_from_secret_ref() {
        let mut replies = session(vec![mounts(), resolution()]);
        replies.extend(session(vec![Value::Null]));
        replies.extend(session(vec![Value::Null, mounts(), resolution()]));
        let (s, calls) = sink(replies);
        let change = MountChange {
            password_secret: Some("homeassistant.willow.smb".into()),
            ..Default::default()
        };
        let r = with_cap_sink(s, || {
            block_on(
                client().mount_repair("willow_media", &change, true, |name| {
                    assert_eq!(name, "homeassistant.willow.smb");
                    Ok(plugin_toolkit::secrets::Secret::new("n3w-pass".to_string()))
                }),
            )
        })
        .unwrap();
        let out = plugin_toolkit::serde_json::to_string(&r).unwrap();
        assert!(!out.contains("n3w-pass"), "{out}");
        assert!(out.contains("<secret homeassistant.willow.smb>"), "{out}");
        let put = &sent(&calls)[4];
        assert_eq!(put["method"], "put");
        assert_eq!(put["endpoint"], "/mounts/willow_media");
        assert_eq!(
            put["data"],
            json!({
                "type": "cifs", "usage": "media", "server": "10.0.0.12", "share": "media",
                "read_only": true, "version": "auto", "username": "ha", "password": "n3w-pass"
            })
        );
    }

    #[test]
    fn cifs_update_error_never_echoes_password() {
        let mut replies = session(vec![mounts(), resolution()]);
        replies.extend(vec![
            json!({"t": "open", "session": 1}),
            frame(json!({"type": "auth_required"})),
            Value::Null,
            frame(json!({"type": "auth_ok"})),
            Value::Null,
            frame(json!({"id": 1, "type": "result", "success": false,
                "error": {"message": "invalid password n3w-pass for mount"}})),
            Value::Null,
        ]);
        let (s, _) = sink(replies);
        let change = MountChange {
            password_secret: Some("homeassistant.willow.smb".into()),
            ..Default::default()
        };
        let err = with_cap_sink(s, || {
            block_on(client().mount_repair("willow_media", &change, true, |_| {
                Ok(plugin_toolkit::secrets::Secret::new("n3w-pass".to_string()))
            }))
        })
        .unwrap_err();
        assert!(!format!("{err:#}").contains("n3w-pass"), "{err:#}");
    }

    #[test]
    fn cifs_update_without_secret_is_refused() {
        let (s, _) = sink(session(vec![mounts(), resolution()]));
        let change = MountChange {
            server: Some("10.0.0.14".into()),
            ..Default::default()
        };
        let err = with_cap_sink(s, || {
            block_on(client().mount_repair("willow_media", &change, false, no_secret))
        })
        .unwrap_err();
        assert!(err.to_string().contains("password_secret"), "{err}");
    }

    #[test]
    fn nfs_rejects_credentials_and_bad_names() {
        let change = MountChange {
            username: Some("x".into()),
            ..Default::default()
        };
        let raw = &mounts()["mounts"][0];
        assert!(update_body(raw, &change, None).is_err());
        for bad in ["", "1abc", "a/b", "../x", "a b"] {
            assert!(mount_name(bad).is_err(), "{bad}");
        }
        assert!(find_raw(&mounts(), "nope").is_err());
    }

    fn failed_session(message: &str) -> Vec<Value> {
        vec![
            json!({"t": "open", "session": 1}),
            frame(json!({"type": "auth_required"})),
            Value::Null,
            frame(json!({"type": "auth_ok"})),
            Value::Null,
            frame(json!({"id": 1, "type": "result", "success": false,
                "error": {"message": message}})),
            Value::Null,
        ]
    }

    #[test]
    fn applied_update_with_failed_reload_reports_partial_result() {
        let mut changed = mounts();
        changed["mounts"][1]["server"] = json!("10.0.0.14");
        changed["mounts"][1]["state"] = json!("failed");
        let mut replies = session(vec![mounts(), resolution()]);
        replies.extend(session(vec![Value::Null]));
        replies.extend(failed_session("mount error n3w-pass rejected"));
        replies.extend(session(vec![changed, resolution()]));
        let (s, _) = sink(replies);
        let change = MountChange {
            server: Some("10.0.0.14".into()),
            password_secret: Some("homeassistant.willow.smb".into()),
            ..Default::default()
        };
        let r = with_cap_sink(s, || {
            block_on(client().mount_repair("willow_media", &change, true, |_| {
                Ok(plugin_toolkit::secrets::Secret::new("n3w-pass".to_string()))
            }))
        })
        .unwrap();
        assert!(r.applied);
        let err = r.reload_error.as_deref().unwrap();
        assert!(
            err.contains("<redacted>") && !err.contains("n3w-pass"),
            "{err}"
        );
        assert_eq!(r.before.server, "10.0.0.12");
        let after = r.after.unwrap();
        assert_eq!(after.server, "10.0.0.14");
        assert!(after.failed);
    }

    #[test]
    fn heal_defaults_to_plan_and_targets_only_unhealthy_mounts() {
        let (s, calls) = sink(session(vec![mounts(), resolution()]));
        let r = with_cap_sink(s, || block_on(client().mount_heal(false))).unwrap();
        assert!(r.dry_run);
        assert_eq!(r.plan, ["POST /mounts/maple_backups/reload"]);
        assert!(sent(&calls)
            .iter()
            .all(|m| m["method"].is_null() || m["method"] == "get"));
    }

    #[test]
    fn heal_reloads_unhealthy_mounts_and_is_noop_when_healthy() {
        let mut healthy = mounts();
        healthy["mounts"][0]["state"] = json!("active");
        let mut replies = session(vec![mounts(), resolution()]);
        replies.extend(session(vec![Value::Null]));
        replies.extend(session(vec![
            healthy.clone(),
            json!({"issues": [], "suggestions": []}),
        ]));
        let (s, calls) = sink(replies);
        let r = with_cap_sink(s, || block_on(client().mount_heal(true))).unwrap();
        assert!(!r.dry_run);
        assert_eq!(r.mounts.len(), 1);
        assert!(r.mounts[0].error.is_none());
        assert!(!r.mounts[0].after.as_ref().unwrap().failed);
        assert_eq!(
            sent(&calls)[4],
            json!({"id": 1, "type": "supervisor/api", "endpoint": "/mounts/maple_backups/reload", "method": "post"})
        );

        let (s, calls) = sink(session(vec![
            healthy,
            json!({"issues": [], "suggestions": []}),
        ]));
        let r = with_cap_sink(s, || block_on(client().mount_heal(true))).unwrap();
        assert!(r.mounts.is_empty() && r.plan.is_empty());
        assert!(sent(&calls)
            .iter()
            .all(|m| m["method"].is_null() || m["method"] == "get"));
    }
}
