use mockito::{Matcher, Server};

use super::*;
use crate::ward::agents::contract::{Passport, SessionStatus};

fn register_request() -> RegisterRequest {
    RegisterRequest {
        key: "agk_key".to_owned(),
        name: "steep-pc".to_owned(),
        os: "windows".to_owned(),
        arch: "x86_64".to_owned(),
        ward_version: "v1".to_owned(),
        passport: Passport::default(),
    }
}

#[tokio::test]
async fn register_and_errors() {
    let mut server = Server::new_async().await;
    let client = http_client::Client::new_for_test();
    let base_url = format!("{}/agents", server.url());
    let api = Api {
        client: &client,
        base_url: &base_url,
    };
    let ok = server
        .mock("POST", "/agents/api/v1/worker/register")
        .match_header("content-type", "application/json")
        .match_body(Matcher::PartialJson(
            serde_json::json!({"key": "agk_key", "name": "steep-pc"}),
        ))
        .with_body(r#"{"workerId":"w1","token":"agw_t","heartbeatSec":15}"#)
        .expect(1)
        .create_async()
        .await;
    let response = api.register(&register_request()).await.unwrap();
    assert_eq!(response.worker_id, "w1");
    assert_eq!(response.token, "agw_t");
    ok.assert_async().await;
    ok.remove_async().await;

    let rejected = server
        .mock("POST", "/agents/api/v1/worker/register")
        .with_status(401)
        .create_async()
        .await;
    assert_eq!(
        api.register(&register_request()).await.unwrap_err(),
        ApiError::Unauthorized
    );
    rejected.remove_async().await;

    server
        .mock("POST", "/agents/api/v1/worker/register")
        .with_status(503)
        .create_async()
        .await;
    assert_eq!(
        api.register(&register_request()).await.unwrap_err(),
        ApiError::Status(503)
    );
}

#[tokio::test]
async fn worker_calls_carry_the_token() {
    let mut server = Server::new_async().await;
    let client = http_client::Client::new_for_test();
    let base_url = server.url();
    let api = Api {
        client: &client,
        base_url: &base_url,
    };
    let inbox = server
        .mock("GET", "/api/v1/worker/inbox")
        .match_query(Matcher::UrlEncoded("wait".into(), "1".into()))
        .match_header("authorization", "Bearer agw_t")
        .with_status(204)
        .create_async()
        .await;
    assert_eq!(api.inbox("agw_t", 1).await.unwrap(), Inbox::default());
    inbox.assert_async().await;

    let ack = server
        .mock("POST", "/api/v1/worker/assignments/s%201/ack")
        .match_header("authorization", "Bearer agw_t")
        .match_body(Matcher::Json(
            serde_json::json!({"accepted": false, "reason": "busy"}),
        ))
        .with_status(204)
        .create_async()
        .await;
    api.ack("agw_t", "s 1", &AckRequest::reject("busy"))
        .await
        .unwrap();
    ack.assert_async().await;

    let journal = server
        .mock("POST", "/api/v1/worker/sessions/s1/journal")
        .match_query(Matcher::UrlEncoded("seq".into(), "7".into()))
        .match_header("content-type", "application/x-ndjson")
        .match_body("{\"a\":1}\n")
        .with_status(204)
        .create_async()
        .await;
    api.journal("agw_t", "s1", 7, b"{\"a\":1}\n".to_vec())
        .await
        .unwrap();
    journal.assert_async().await;

    let status = server
        .mock("POST", "/api/v1/worker/sessions/s1/status")
        .match_body(Matcher::PartialJson(
            serde_json::json!({"status": "review", "exitCode": 0}),
        ))
        .with_status(204)
        .create_async()
        .await;
    let mut request = StatusRequest::new(SessionStatus::Review);
    request.exit_code = Some(0);
    api.status("agw_t", "s1", &request).await.unwrap();
    status.assert_async().await;

    server
        .mock("POST", "/api/v1/worker/heartbeat")
        .with_status(401)
        .create_async()
        .await;
    let heartbeat = HeartbeatRequest {
        resources: Default::default(),
        usage: Vec::new(),
        sessions: Vec::new(),
    };
    assert_eq!(
        api.heartbeat("agw_old", &heartbeat).await.unwrap_err(),
        ApiError::Unauthorized
    );
}

#[tokio::test]
async fn unreachable_service_is_a_network_error() {
    let client = http_client::Client::new_for_test();
    let api = Api {
        client: &client,
        base_url: "http://127.0.0.1:9",
    };
    assert!(matches!(api.inbox("t", 0).await, Err(ApiError::Network(_))));
}

#[test]
fn path_segments_are_encoded() {
    assert_eq!(path_segment("5f0e8a5b-0c3e"), "5f0e8a5b-0c3e");
    assert_eq!(path_segment("a/b c"), "a%2Fb%20c");
}
