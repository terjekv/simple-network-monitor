mod auth;
mod dto;
mod errors;
mod openapi;
pub(crate) mod routes;

use crate::{
    config::ModuleConfigs,
    domain::ApiToken,
    storage::{HostRepository, IcmpRepository, UsageRepository},
};
use actix_web::web;
use std::sync::{Arc, RwLock};

pub use errors::ApiError;

#[derive(Clone)]
pub struct ApiState {
    pub hosts: Arc<dyn HostRepository>,
    pub icmp: Arc<dyn IcmpRepository>,
    pub usage: Arc<dyn UsageRepository>,
    pub api_token: Arc<RwLock<Option<ApiToken>>>,
    pub module_config: Arc<RwLock<ModuleConfigs>>,
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    routes::configure(cfg);
    openapi::configure(cfg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{Host, HostRuntimeState, HostStatus, UsageSnapshot},
        storage::SqliteStorage,
    };
    use actix_web::{App, http::StatusCode, test as actix_test};
    use std::sync::Arc;

    fn host_fixture() -> Host {
        Host {
            id: "r1".into(),
            address: "192.0.2.1".into(),
            name: "Router 1".into(),
            groups: vec!["core".into()],
            metadata: Default::default(),
            modules: Default::default(),
        }
    }

    fn api_state(storage: Arc<SqliteStorage>, api_token: Option<&str>) -> ApiState {
        ApiState {
            hosts: storage.clone(),
            icmp: storage.clone(),
            usage: storage,
            api_token: Arc::new(RwLock::new(api_token.map(crate::domain::ApiToken::from))),
            module_config: Arc::new(RwLock::new(ModuleConfigs::default())),
        }
    }

    #[actix_web::test]
    async fn lists_hosts_as_json() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        storage
            .update_check_result("r1", HostRuntimeState::default(), None)
            .await
            .unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get().uri("/v1/hosts").to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn filters_hosts_by_status_and_group() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let state = HostRuntimeState {
            status: HostStatus::Down,
            ..Default::default()
        };
        storage
            .update_check_result("r1", state, None)
            .await
            .unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts?icmp.status=down&group=core")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn filters_hosts_by_metadata() {
        let mut r1 = host_fixture();
        r1.metadata
            .insert("room".into(), serde_json::json!("net-a"));
        let mut r2 = host_fixture();
        r2.id = "r2".into();
        r2.metadata
            .insert("room".into(), serde_json::json!("lab-2"));
        let storage = Arc::new(SqliteStorage::in_memory(vec![r1, r2]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts?metadata.room=net-a")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["id"], "r1");
    }

    #[actix_web::test]
    async fn rejects_duplicate_query_keys() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts?metadata.room=net-a&metadata.room=lab-2")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn exposes_namespace_catalog() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/namespaces")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert!(body["namespaces"]["icmp"]["status"].is_object());
        assert!(body["namespaces"]["usage"]["no_users_for"].is_object());
    }

    #[actix_web::test]
    async fn exposes_module_catalog() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/modules")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert_eq!(body["modules"].as_array().unwrap().len(), 2);
        assert!(
            body["modules"].as_array().unwrap().iter().any(|module| {
                module["id"] == "icmp" && module["filters"]["status"].is_object()
            })
        );
        assert!(body["modules"].as_array().unwrap().iter().any(|module| {
            module["id"] == "usage"
                && module["config_options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|option| option["key"] == "sample_retention")
        }));
    }

    #[actix_web::test]
    async fn exposes_openapi_document() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/openapi.json")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert!(body["openapi"].as_str().is_some());
        assert!(body["paths"]["/v1/hosts"].is_object());
        assert!(body["paths"]["/v1/hosts/{id}/usage/samples"].is_object());
        let host_params = body["paths"]["/v1/hosts"]["get"]["parameters"]
            .as_array()
            .unwrap();
        assert!(
            host_params
                .iter()
                .any(|param| param["name"] == "usage.no_users_for")
        );
        assert!(
            host_params
                .iter()
                .any(|param| param["name"] == "icmp.status")
        );
    }

    #[actix_web::test]
    async fn requires_bearer_token_when_configured() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, Some("secret"))))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get().uri("/v1/hosts").to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn exposes_usage_summary() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        storage
            .update_usage("r1", UsageSnapshot::success(chrono::Utc::now(), 1, 2))
            .await
            .unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get().uri("/v1/usage").to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn filters_usage_by_inactive_console_duration() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        storage
            .update_usage("r1", UsageSnapshot::success(chrono::Utc::now(), 0, 1))
            .await
            .unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts?usage.inactive_console_for=1h")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn rejects_invalid_usage_filter_duration() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/usage?usage.no_users_for=definitely-not-a-duration")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn rejects_duplicate_usage_query_keys() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/usage?usage.status=ok&usage.status=error")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn host_history_returns_404_for_unknown_host() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts/nope/history")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn host_usage_history_returns_404_for_unknown_host() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts/nope/usage/history")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn host_usage_samples_returns_404_for_unknown_host() {
        let storage = Arc::new(SqliteStorage::in_memory(vec![host_fixture()]).unwrap());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api_state(storage, None)))
                .configure(configure),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/v1/hosts/nope/usage/samples")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
