//! Home Assistant WebSocket API client (`/api/websocket`).
//!
//! Registry, exposure, assist, backup, and Supervisor passthrough
//! (`supervisor/api`) are WebSocket-only. [`HaWs`] owns the protocol: the
//! auth handshake, per-request ids, and matching each `result` to its request
//! while skipping interleaved events. The socket itself sits behind
//! [`WsTransport`].
//!
//! [`connect`] fails until `plugin-toolkit` exposes a WebSocket client: this
//! plugin reaches external crates only through the toolkit, and the toolkit
//! has no raw WebSocket transport yet (only Socket.IO).
#![allow(clippy::disallowed_types)]

use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json::{json, Map, Value};

use crate::Config;

/// Text-frame transport under [`HaWs`].
pub(crate) trait WsTransport {
    async fn send(&mut self, text: String) -> Result<()>;
    async fn recv(&mut self) -> Result<String>;
}

/// Frames read while waiting for one result before giving up; bounds a
/// connection that streams events but never answers.
const MAX_FRAMES_PER_CALL: usize = 10_000;

pub(crate) struct HaWs<T> {
    t: T,
    next_id: u64,
}

impl<T: WsTransport> HaWs<T> {
    /// Runs the auth handshake: `auth_required` → `auth` → `auth_ok`.
    pub(crate) async fn handshake(mut t: T, token: &str) -> Result<Self> {
        let first = parse(&t.recv().await?)?;
        if first["type"] != "auth_required" {
            bail!("expected auth_required, got {}", first["type"]);
        }
        t.send(json!({"type": "auth", "access_token": token}).to_string())
            .await?;
        let reply = parse(&t.recv().await?)?;
        match reply["type"].as_str() {
            Some("auth_ok") => Ok(Self { t, next_id: 1 }),
            Some("auth_invalid") => bail!(
                "Home Assistant rejected the token: {}",
                reply["message"].as_str().unwrap_or("auth_invalid")
            ),
            other => bail!("unexpected auth reply {other:?}"),
        }
    }

    /// Sends `{"type": kind, ..fields}` and returns its `result`. An error
    /// result becomes `Err` carrying HA's code and message.
    pub(crate) async fn call(&mut self, kind: &str, fields: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = match fields {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            other => bail!("call fields must be an object, got {other}"),
        };
        msg.insert("id".into(), id.into());
        msg.insert("type".into(), kind.into());
        self.t.send(Value::Object(msg).to_string()).await?;
        for _ in 0..MAX_FRAMES_PER_CALL {
            let v = parse(&self.t.recv().await?)?;
            if v["id"].as_u64() != Some(id) || v["type"] != "result" {
                continue;
            }
            if v["success"].as_bool() == Some(true) {
                return Ok(v["result"].clone());
            }
            bail!(
                "{kind} failed: {} ({})",
                v["error"]["message"].as_str().unwrap_or("unknown error"),
                v["error"]["code"].as_str().unwrap_or("no code")
            );
        }
        bail!("{kind}: no result after {MAX_FRAMES_PER_CALL} frames")
    }
}

fn parse(text: &str) -> Result<Value> {
    plugin_toolkit::serde_json::from_str(text)
        .context("invalid WebSocket frame from Home Assistant")
}

/// `ws(s)://host/api/websocket` for an endpoint's REST base URL.
pub(crate) fn websocket_url(base_url: &str) -> Result<String> {
    let base = base_url.trim_end_matches('/');
    let rest = if let Some(r) = base.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = base.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        bail!("endpoint base_url '{base_url}' must start with http:// or https://");
    };
    Ok(format!("{rest}/api/websocket"))
}

/// Placeholder transport type until the toolkit provides a real one.
pub(crate) enum Unavailable {}

impl WsTransport for Unavailable {
    async fn send(&mut self, _: String) -> Result<()> {
        match *self {}
    }
    async fn recv(&mut self) -> Result<String> {
        match *self {}
    }
}

/// Opens the socket. Fails until the toolkit provides a WebSocket client.
fn open_transport(url: &str) -> Result<Unavailable> {
    bail!(
        "WebSocket transport unavailable: plugin-toolkit has no WebSocket client yet, \
         so {url} cannot be opened"
    )
}

/// Opens an authenticated session to `cfg`.
pub(crate) async fn connect(cfg: &Config) -> Result<HaWs<Unavailable>> {
    let t = open_transport(&websocket_url(&cfg.base_url)?)?;
    HaWs::handshake(t, &cfg.token).await
}

#[cfg(test)]
pub(crate) mod testing {
    //! A scripted Home Assistant: answers each request by `type` from a table,
    //! echoing the request id, and records every request it saw.
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    pub(crate) type Handler = Box<dyn Fn(&Value) -> Value + Send>;

    #[derive(Default)]
    pub(crate) struct FakeHa {
        handlers: HashMap<String, Handler>,
        queue: VecDeque<String>,
        pub(crate) seen: Arc<Mutex<Vec<Value>>>,
        authed: bool,
    }

    impl FakeHa {
        pub(crate) fn new() -> Self {
            let mut f = Self::default();
            f.queue
                .push_back(json!({"type": "auth_required"}).to_string());
            f
        }

        /// `handler` returns the `result` payload, or `{"__error": msg}` for an error result.
        pub(crate) fn on(
            mut self,
            kind: &str,
            handler: impl Fn(&Value) -> Value + Send + 'static,
        ) -> Self {
            self.handlers.insert(kind.into(), Box::new(handler));
            self
        }

        pub(crate) async fn session(self) -> HaWs<FakeHa> {
            HaWs::handshake(self, "tok").await.unwrap()
        }
    }

    impl WsTransport for FakeHa {
        async fn send(&mut self, text: String) -> Result<()> {
            let v: Value = plugin_toolkit::serde_json::from_str(&text)?;
            if !self.authed {
                self.authed = true;
                let ok = v["access_token"] == "tok";
                self.queue.push_back(
                    json!({"type": if ok { "auth_ok" } else { "auth_invalid" }, "message": "bad"})
                        .to_string(),
                );
                return Ok(());
            }
            self.seen.lock().unwrap().push(v.clone());
            let kind = v["type"].as_str().unwrap_or_default().to_string();
            let id = v["id"].clone();
            // An unrelated event first, to prove results are matched by id.
            self.queue
                .push_back(json!({"id": 999, "type": "event", "event": {}}).to_string());
            let reply = match self.handlers.get(&kind) {
                Some(h) => match h(&v) {
                    Value::Object(m) if m.contains_key("__error") => json!({
                        "id": id, "type": "result", "success": false,
                        "error": {"code": "fake_error", "message": m["__error"]}
                    }),
                    r => json!({"id": id, "type": "result", "success": true, "result": r}),
                },
                None => json!({
                    "id": id, "type": "result", "success": false,
                    "error": {"code": "unknown_command", "message": "Unknown command."}
                }),
            };
            self.queue.push_back(reply.to_string());
            Ok(())
        }

        async fn recv(&mut self) -> Result<String> {
            self.queue.pop_front().context("fake: connection closed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeHa;
    use super::*;

    #[tokio::test]
    async fn handshake_and_call_match_results_by_id() {
        let mut ws = FakeHa::new()
            .on("ping_me", |v| json!({"echo": v["x"]}))
            .session()
            .await;
        assert_eq!(
            ws.call("ping_me", json!({"x": 1})).await.unwrap()["echo"],
            1
        );
        assert_eq!(
            ws.call("ping_me", json!({"x": 2})).await.unwrap()["echo"],
            2
        );
    }

    #[tokio::test]
    async fn error_results_carry_code_and_message() {
        let mut ws = FakeHa::new()
            .on("boom", |_| json!({"__error": "nope"}))
            .session()
            .await;
        let err = ws.call("boom", Value::Null).await.unwrap_err().to_string();
        assert!(err.contains("nope") && err.contains("fake_error"), "{err}");
        let err = ws
            .call("missing", Value::Null)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown_command"), "{err}");
    }

    #[tokio::test]
    async fn bad_token_is_rejected() {
        let err = HaWs::handshake(FakeHa::new(), "wrong").await.err().unwrap();
        assert!(err.to_string().contains("rejected"), "{err}");
    }

    #[test]
    fn websocket_url_maps_scheme() {
        assert_eq!(
            websocket_url("https://ha.example:8123/").unwrap(),
            "wss://ha.example:8123/api/websocket"
        );
        assert_eq!(
            websocket_url("http://10.0.0.5:8123").unwrap(),
            "ws://10.0.0.5:8123/api/websocket"
        );
        assert!(websocket_url("ftp://x").is_err());
    }

    #[tokio::test]
    async fn connect_reports_missing_transport() {
        let err = connect(&Config::new("http://10.0.0.5:8123", "t"))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("transport unavailable"), "{err}");
    }
}
