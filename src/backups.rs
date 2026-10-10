//! Home Assistant's own backups, surfaced for fleet review.
//!
//! HA manages its backups itself, outside any PBS job, so "is HA backed up?"
//! is answered here: the backup list, the age of the newest one, and whether
//! any copy lives off the box it protects. A missing or stale backup is
//! reported as an alert, not just an absent row.
//!
//! Source: the Supervisor `/backups` list through the WebSocket
//! `supervisor/api` passthrough, which carries type and locations and accepts
//! a long-lived token (the REST `/api/hassio/*` proxy returns 401 for one).
//! Installs without a Supervisor fall back to core `backup/info`.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Value};
use plugin_toolkit::time::Timestamp;

use crate::ws::{HaWs, WsTransport};

/// HA's own default for `days_until_stale`.
pub const DEFAULT_STALE_DAYS: u64 = 3;

/// Location names that mean "inside the HA host".
const LOCAL_LOCATIONS: &[&str] = &["", ".local", "local", "hassio.local", "backup.local"];

#[derive(
    Debug,
    Clone,
    PartialEq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct HaBackup {
    /// Supervisor slug or core backup_id.
    pub id: String,
    pub name: String,
    /// RFC 3339.
    pub date: String,
    pub age_hours: Option<u64>,
    pub size_bytes: Option<u64>,
    /// `full`, `partial`, or `unknown` (core installs do not report it).
    pub kind: String,
    /// Where copies are stored: `local`, a Supervisor mount name, or a
    /// backup agent id.
    pub locations: Vec<String>,
    /// At least one copy is outside the HA host.
    pub off_box: bool,
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    /// `supervisor` or `core`.
    pub source: String,
    /// `ok`, `stale`, or `missing`.
    pub status: String,
    pub newest_age_hours: Option<u64>,
    /// Age of the newest backup with an off-box copy; `None` when there is none.
    pub newest_off_box_age_hours: Option<u64>,
    pub stale_after_hours: u64,
    /// One line per problem; empty when healthy.
    pub alerts: Vec<String>,
    /// Newest first.
    pub backups: Vec<HaBackup>,
}

fn age_hours(date: &str, now: i64) -> Option<u64> {
    let t = Timestamp::parse_rfc3339(date).ok()?.unix_seconds();
    Some(((now - t).max(0) / 3600) as u64)
}

fn is_local(loc: &str) -> bool {
    LOCAL_LOCATIONS.contains(&loc)
}

/// Parses the Supervisor `/backups` payload.
pub fn from_supervisor(data: &Value, now: i64) -> (Vec<HaBackup>, Option<u64>) {
    let backups = data["backups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let id = b["slug"].as_str()?.to_string();
            let date = b["date"].as_str().unwrap_or_default().to_string();
            let mut locations: Vec<String> = match b["locations"].as_array() {
                Some(ls) => ls
                    .iter()
                    .map(|l| l.as_str().unwrap_or("").to_string())
                    .collect(),
                None => vec![b["location"].as_str().unwrap_or("").to_string()],
            };
            for l in &mut locations {
                if is_local(l) {
                    *l = "local".into();
                }
            }
            locations.sort();
            locations.dedup();
            let size_bytes = b["size_bytes"]
                .as_u64()
                .or_else(|| b["size"].as_f64().map(|mb| (mb * 1024.0 * 1024.0) as u64));
            Some(HaBackup {
                off_box: locations.iter().any(|l| l != "local"),
                age_hours: age_hours(&date, now),
                kind: b["type"].as_str().unwrap_or("unknown").to_string(),
                name: b["name"].as_str().unwrap_or_default().to_string(),
                id,
                date,
                size_bytes,
                locations,
            })
        })
        .collect();
    (backups, data["days_until_stale"].as_u64())
}

/// Parses core `backup/info` (no Supervisor).
pub fn from_core(info: &Value, now: i64) -> Vec<HaBackup> {
    info["backups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let id = b["backup_id"].as_str()?.to_string();
            let date = b["date"].as_str().unwrap_or_default().to_string();
            let agents = b["agents"].as_object();
            let mut locations: Vec<String> = agents
                .map(|m| {
                    m.keys()
                        .map(|k| {
                            if is_local(k) {
                                "local".into()
                            } else {
                                k.clone()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            locations.sort();
            locations.dedup();
            let size_bytes =
                agents.and_then(|m| m.values().filter_map(|a| a["size"].as_u64()).max());
            Some(HaBackup {
                off_box: locations.iter().any(|l| l != "local"),
                age_hours: age_hours(&date, now),
                kind: "unknown".into(),
                name: b["name"].as_str().unwrap_or_default().to_string(),
                id,
                date,
                size_bytes,
                locations,
            })
        })
        .collect()
}

pub fn report(source: &str, mut backups: Vec<HaBackup>, stale_after_hours: u64) -> BackupReport {
    backups.sort_by_key(|b| b.age_hours.unwrap_or(u64::MAX));
    let newest = backups.first().and_then(|b| b.age_hours);
    let newest_off_box = backups
        .iter()
        .filter(|b| b.off_box)
        .find_map(|b| b.age_hours);
    let mut alerts = Vec::new();
    let status = match newest {
        None if backups.is_empty() => {
            alerts.push("no Home Assistant backups exist".into());
            "missing"
        }
        None => {
            alerts.push("no backup has a readable date".into());
            "stale"
        }
        Some(h) if h > stale_after_hours => {
            alerts.push(format!(
                "newest Home Assistant backup is {} days old",
                h / 24
            ));
            "stale"
        }
        Some(_) => "ok",
    };
    match newest_off_box {
        None if !backups.is_empty() => alerts.push(
            "no backup has an off-box copy; a backup inside the VM it protects is not a backup"
                .into(),
        ),
        Some(h) if h > stale_after_hours => {
            alerts.push(format!("newest off-box backup is {} days old", h / 24))
        }
        _ => {}
    }
    BackupReport {
        source: source.into(),
        status: status.into(),
        newest_age_hours: newest,
        newest_off_box_age_hours: newest_off_box,
        stale_after_hours,
        alerts,
        backups,
    }
}

pub(crate) async fn collect<T: WsTransport>(
    ws: &mut HaWs<T>,
    stale_days: Option<u64>,
    now: i64,
) -> Result<BackupReport> {
    match ws
        .call(
            "supervisor/api",
            json!({"endpoint": "/backups", "method": "get"}),
        )
        .await
    {
        Ok(data) => {
            let (backups, ha_days) = from_supervisor(&data, now);
            let days = stale_days.or(ha_days).unwrap_or(DEFAULT_STALE_DAYS);
            Ok(report("supervisor", backups, days * 24))
        }
        Err(sup) => {
            let info = ws.call("backup/info", Value::Null).await.with_context(|| {
                format!("Supervisor unavailable ({sup}) and backup/info failed")
            })?;
            let mut r = report(
                "core",
                from_core(&info, now),
                stale_days.unwrap_or(DEFAULT_STALE_DAYS) * 24,
            );
            let attempted = info["last_attempted_automatic_backup"].as_str();
            let completed = info["last_completed_automatic_backup"].as_str();
            if let (Some(a), c) = (attempted, completed) {
                if c.is_none_or(|c| a > c) {
                    r.alerts.push(format!(
                        "last automatic backup attempt ({a}) did not complete"
                    ));
                }
            }
            Ok(r)
        }
    }
}

/// **Review Home Assistant's own backups** (read-only): every backup with
/// date, size, type, and locations; a `missing` / `stale` / `ok` status; the
/// age of the newest off-box copy; and alerts for anything unhealthy.
#[endpoint_tool(domain = "home-assistant", verb = "backups", resolve = crate::tools::endpoint_config)]
async fn ha_backups(
    cfg: crate::Config,
    /// Override HA's `days_until_stale`.
    #[arg(long)]
    stale_days: Option<u64>,
) -> Result<BackupReport> {
    let mut ws = crate::ws::connect(&cfg).await?;
    collect(
        &mut ws,
        stale_days,
        plugin_toolkit::time::now().unix_seconds(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::testing::FakeHa;

    // 2026-09-25T12:00:00Z
    const NOW: i64 = 1_790_337_600;

    fn sup() -> Value {
        json!({"days_until_stale": 3, "backups": [
            {"slug": "a1", "name": "Automatic", "date": "2026-09-24T12:00:00+00:00", "type": "full",
             "size": 1.5, "location": null, "locations": [null, "willow"]},
            {"slug": "b2", "name": "pre-update", "date": "2026-09-20T12:00:00+00:00", "type": "partial",
             "size_bytes": 2048, "location": null}
        ]})
    }

    #[test]
    fn supervisor_backups_parse_type_size_and_off_box() {
        let (b, days) = from_supervisor(&sup(), NOW);
        assert_eq!(days, Some(3));
        assert_eq!(b[0].kind, "full");
        assert_eq!(b[0].locations, ["local", "willow"]);
        assert!(b[0].off_box);
        assert_eq!(b[0].age_hours, Some(24));
        assert_eq!(b[0].size_bytes, Some(1_572_864));
        assert!(!b[1].off_box);
        assert_eq!(b[1].size_bytes, Some(2048));
    }

    #[test]
    fn healthy_report_has_no_alerts() {
        let (b, _) = from_supervisor(&sup(), NOW);
        let r = report("supervisor", b, 72);
        assert_eq!(r.status, "ok");
        assert_eq!(r.newest_off_box_age_hours, Some(24));
        assert!(r.alerts.is_empty(), "{:?}", r.alerts);
    }

    #[test]
    fn stale_missing_and_local_only_are_loud() {
        let (b, _) = from_supervisor(&sup(), NOW + 10 * 86_400);
        let r = report("supervisor", b, 72);
        assert_eq!(r.status, "stale");
        assert!(
            r.alerts.iter().any(|a| a.contains("11 days old")),
            "{:?}",
            r.alerts
        );

        let r = report("supervisor", vec![], 72);
        assert_eq!(r.status, "missing");

        let local_only =
            json!({"backups": [{"slug": "x", "date": "2026-09-25T00:00:00Z", "location": null}]});
        let (b, _) = from_supervisor(&local_only, NOW);
        let r = report("supervisor", b, 72);
        assert_eq!(r.status, "ok");
        assert!(r.alerts.iter().any(|a| a.contains("off-box")));
    }

    #[test]
    fn core_backup_info_parses_agents() {
        let info = json!({"backups": [{"backup_id": "c1", "name": "Auto", "date": "2026-09-25T06:00:00Z",
            "agents": {"backup.local": {"size": 10}, "cloud.cloud": {"size": 12}}}]});
        let b = from_core(&info, NOW);
        assert_eq!(b[0].locations, ["cloud.cloud", "local"]);
        assert!(b[0].off_box);
        assert_eq!(b[0].size_bytes, Some(12));
        assert_eq!(b[0].kind, "unknown");
    }

    #[tokio::test]
    async fn uses_supervisor_when_present() {
        let mut ws = FakeHa::new()
            .on("supervisor/api", |v| {
                assert_eq!(v["endpoint"], "/backups");
                sup()
            })
            .session()
            .await;
        let r = collect(&mut ws, None, NOW).await.unwrap();
        assert_eq!((r.source.as_str(), r.stale_after_hours), ("supervisor", 72));
        assert_eq!(r.backups.len(), 2);
    }

    #[tokio::test]
    async fn falls_back_to_core_and_flags_failed_automatic_backup() {
        let mut ws = FakeHa::new()
            .on("backup/info", |_| {
                json!({"backups": [], "last_attempted_automatic_backup": "2026-09-25T03:00:00Z",
                       "last_completed_automatic_backup": "2026-09-14T03:00:00Z"})
            })
            .session()
            .await;
        let r = collect(&mut ws, Some(2), NOW).await.unwrap();
        assert_eq!(r.source, "core");
        assert_eq!(r.status, "missing");
        assert_eq!(r.stale_after_hours, 48);
        assert!(r.alerts.iter().any(|a| a.contains("did not complete")));
    }
}
