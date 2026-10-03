use super::*;
use crate::{
    AppConfig,
    app::telemetry::RuntimeMetrics,
    domain::{
        Host, HostRuntimeState, UsageSnapshot,
        tcp::{CheckId, TcpSnapshot},
    },
    storage::{HostRepository, IcmpRepository, SqliteStorage, TcpRepository, UsageRepository},
};
use actix_web::{App, http::StatusCode, test as actix_test};
use std::sync::{Arc, RwLock};

fn host() -> Host {
    AppConfig::from_toml_str(r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["example"], modules.tcp.checks = [{ id = "web", port = 443 }] }]
[modules.usage]
enabled = true
[modules.tcp]
enabled = true
"#).unwrap().hosts.remove(0)
}

fn state(storage: Arc<SqliteStorage>) -> ApiState {
    ApiState {
        hosts: storage.clone(),
        icmp: storage.clone(),
        history: storage.clone(),
        usage: storage,
        metrics: Arc::new(RuntimeMetrics::default()),
        api_token: Arc::new(RwLock::new(Some(
            crate::domain::ApiToken::new("fake-token").unwrap(),
        ))),
        module_config: Arc::new(RwLock::new(Default::default())),
    }
}

#[rstest::rstest]
#[case(None)]
#[case(Some(-1000))]
#[case(Some(60))]
fn missing_stale_and_future_observations_are_not_fresh(#[case] offset: Option<i64>) {
    let now = Utc::now();
    let state = HostRuntimeState {
        last_checked_at: offset.map(|s| now + chrono::Duration::seconds(s)),
        ..Default::default()
    };
    let output = render(&[state.to_record(&host(), None)], &Default::default(), now);
    assert!(output.contains("snm_check_fresh{host_id=\"r1\",check_id=\"icmp\"} 0\n"));
    assert!(!output.contains("snm_usage_users{"));
    assert!(!output.contains("snm_tcp_connect_success{"));
}

#[actix_web::test]
async fn failed_usage_is_not_reported_as_zero_users() {
    let now = Utc::now();
    let record = HostRuntimeState::default().to_record(
        &host(),
        Some(UsageSnapshot::error(now, "private diagnostic".into())),
    );
    let output = render(&[record], &Default::default(), now);
    assert!(output.contains("snm_usage_collection_success{host_id=\"r1\"} 0\n"));
    assert!(output.contains("snm_check_fresh{host_id=\"r1\",check_id=\"usage\"} 1\n"));
    assert!(!output.contains("snm_usage_users{"));
    assert!(!output.contains("private diagnostic"));
}

#[actix_web::test]
async fn thresholded_status_and_latest_rtt_keep_distinct_meanings() {
    let now = Utc::now();
    let state = HostRuntimeState {
        status: HostStatus::Up,
        last_checked_at: Some(now),
        latency: None,
        consecutive_failures: 1,
        last_error: Some("timeout".into()),
        ..Default::default()
    };
    let output = render(&[state.to_record(&host(), None)], &Default::default(), now);
    assert!(output.contains("snm_icmp_status{host_id=\"r1\",state=\"up\"} 1\n"));
    assert!(!output.contains("snm_icmp_round_trip_seconds{"));
}

#[actix_web::test]
async fn disabled_checks_emit_only_configuration_and_freshness() {
    let now = Utc::now();
    let mut fixture = host();
    fixture.modules.icmp.enabled = false;
    fixture.modules.usage.enabled = false;
    fixture.modules.tcp.enabled = false;
    let output = render(
        &[HostRuntimeState {
            last_checked_at: Some(now),
            ..Default::default()
        }
        .to_record(&fixture, Some(UsageSnapshot::success(now, 0, 0)))],
        &Default::default(),
        now,
    );
    assert!(output.contains("snm_check_enabled{host_id=\"r1\",check_id=\"icmp\"} 0\n"));
    assert!(!output.contains("snm_check_last_observed_timestamp_seconds{"));
    assert!(!output.contains("snm_icmp_status{"));
    assert!(!output.contains("snm_usage_collection_success{"));
}

#[actix_web::test]
async fn metrics_authorization_uses_reloaded_token() {
    let storage = Arc::new(SqliteStorage::in_memory(vec![host()]).unwrap());
    let state = state(storage);
    let token = state.api_token.clone();
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(super::super::configure),
    )
    .await;
    let request = actix_test::TestRequest::get().uri("/metrics").to_request();
    assert_eq!(
        actix_test::call_service(&app, request).await.status(),
        StatusCode::UNAUTHORIZED
    );
    *token.write().unwrap() = Some(crate::domain::ApiToken::new("fake-new-token").unwrap());
    for (value, expected) in [
        ("fake-token", StatusCode::UNAUTHORIZED),
        ("fake-new-token", StatusCode::OK),
    ] {
        let request = actix_test::TestRequest::get()
            .uri("/metrics")
            .insert_header(("Authorization", format!("Bearer {value}")))
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, request).await.status(),
            expected
        );
    }
}

#[actix_web::test]
async fn scrapes_do_not_increment_execution_counters_and_removed_hosts_disappear() {
    let storage = Arc::new(SqliteStorage::in_memory(vec![host()]).unwrap());
    let state = state(storage.clone());
    state
        .metrics
        .observe("tcp", true, Duration::from_millis(50));
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(super::super::configure),
    )
    .await;
    for _ in 0..2 {
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/metrics")
                .insert_header(("Authorization", "Bearer fake-token"))
                .to_request(),
        )
        .await;
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = String::from_utf8(actix_test::read_body(response).await.to_vec()).unwrap();
        assert!(body.contains("snm_check_runs_total{kind=\"tcp\",result=\"success\"} 1\n"));
        assert!(body.contains("snm_check_duration_seconds_bucket{kind=\"tcp\",le=\"+Inf\"} 1\n"));
    }
    storage.update_hosts(vec![]).unwrap();
    let response = actix_test::call_service(
        &app,
        actix_test::TestRequest::get()
            .uri("/metrics")
            .insert_header(("Authorization", "Bearer fake-token"))
            .to_request(),
    )
    .await;
    let body = String::from_utf8(actix_test::read_body(response).await.to_vec()).unwrap();
    assert!(!body.contains("host_id="));
}

#[actix_web::test]
async fn tcp_results_appear_in_host_api_metrics_and_readiness() {
    let storage = Arc::new(SqliteStorage::in_memory(vec![host()]).unwrap());
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state(storage.clone())))
            .configure(super::super::configure),
    )
    .await;
    let get = |path: &str| {
        actix_test::TestRequest::get()
            .uri(path)
            .insert_header(("Authorization", "Bearer fake-token"))
            .to_request()
    };
    assert_eq!(
        actix_test::call_service(&app, get("/readyz"))
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let now = Utc::now();
    storage
        .update_check_result(
            "r1",
            HostRuntimeState {
                last_checked_at: Some(now),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    storage
        .update_usage("r1", UsageSnapshot::success(now, 1, 2))
        .await
        .unwrap();
    assert_eq!(
        actix_test::call_service(&app, get("/readyz"))
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    storage
        .update_tcp(
            "r1",
            &CheckId::new("web".into()).unwrap(),
            TcpSnapshot::new(now, Err("connection refused".into())),
        )
        .await
        .unwrap();
    assert_eq!(
        actix_test::call_service(&app, get("/readyz"))
            .await
            .status(),
        StatusCode::OK
    );
    let response: serde_json::Value =
        actix_test::call_and_read_body_json(&app, get("/v1/hosts/r1")).await;
    assert_eq!(response["tcp"][0]["id"], "web");
    assert_eq!(response["tcp"][0]["observation"]["success"], false);
    assert_eq!(response["tcp"][0]["stale"], false);
    let record = storage.host("r1").await.unwrap().unwrap();
    let output = render(&[record], &Default::default(), now);
    assert!(output.contains("snm_tcp_connect_success{host_id=\"r1\",check_id=\"tcp.web\"} 0\n"));
}
