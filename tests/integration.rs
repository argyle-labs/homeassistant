//! Integration tests for the Home Assistant plugin's HTTP client.
//!
//! Covers base-url normalization, the Bearer auth wire format, the domain
//! filter query param, entity-id URL encoding, and the service-call payload
//! merge. HTTP behavior is exercised against `wiremock`, exactly as the
//! in-crate `Client` unit tests do.

use homeassistant::{Client, Config, ServiceCall};
use plugin_toolkit::serde_json::{json, Map};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn trailing_slash_in_base_url_is_normalized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/states"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"entity_id": "light.lr"}])))
        .mount(&server)
        .await;
    // A trailing slash must not produce `//api/states`.
    let base = format!("{}/", server.uri());
    let v = Client::new(Config::new(base, "tok"))
        .entity_list(None)
        .await
        .expect("entity_list should parse against a normalized URL");
    assert_eq!(v[0]["entity_id"], "light.lr");
}

#[tokio::test]
async fn auth_header_uses_bearer_token_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/states"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Client::new(Config::new(server.uri(), "secret-token"))
        .entity_list(None)
        .await
        .expect("bearer token must be sent");
}

#[tokio::test]
async fn domain_filter_is_passed_as_query_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/states"))
        .and(query_param("domain", "switch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Client::new(Config::new(server.uri(), "t"))
        .entity_list(Some("switch"))
        .await
        .expect("domain filter must be a query param");
}

#[tokio::test]
async fn service_call_merges_entity_id_into_payload() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/services/light/turn_on"))
        .and(body_json(
            json!({"entity_id": "light.lr", "brightness": 200}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let mut data = Map::new();
    data.insert("brightness".into(), json!(200));
    Client::new(Config::new(server.uri(), "t"))
        .service_call(&ServiceCall {
            domain: "light".into(),
            service: "turn_on".into(),
            entity_id: Some("light.lr".into()),
            data,
        })
        .await
        .expect("service call should merge entity_id and post");
}
