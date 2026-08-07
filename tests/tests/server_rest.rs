//! Integration: control plane REST over real TCP + SQLite.
//!
//! Covers Phase 1 exit criteria: server listens; bearer auth on `/v1/status`.

use mc2_tests::TestCluster;
use reqwest::StatusCode;

#[tokio::test]
async fn health_is_public() {
    let cluster = TestCluster::start().await.expect("start cluster");
    let (status, body) = cluster.get_json("/health", None).await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "mc2-server");
}

#[tokio::test]
async fn status_requires_valid_bearer() {
    let cluster = TestCluster::start().await.expect("start cluster");

    let (unauth, _) = cluster.get_json("/v1/status", None).await.unwrap();
    assert_eq!(unauth, StatusCode::UNAUTHORIZED);

    let (bad, _) = cluster
        .get_json("/v1/status", Some("definitely-not-the-token"))
        .await
        .unwrap();
    assert_eq!(bad, StatusCode::UNAUTHORIZED);

    let (ok, body) = cluster
        .get_json("/v1/status", Some(&cluster.api_token))
        .await
        .unwrap();
    assert_eq!(ok, StatusCode::OK);
    assert_eq!(body["api_version"], "mc2/v1");
    assert_eq!(body["nodes_total"], 0);
    assert!(body.get("version").is_some());
}

#[tokio::test]
async fn join_token_is_not_accepted_as_api_bearer() {
    let cluster = TestCluster::start().await.expect("start cluster");
    let (status, _) = cluster
        .get_json("/v1/status", Some(&cluster.join_token))
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "join token must not authorize operator REST"
    );
}
