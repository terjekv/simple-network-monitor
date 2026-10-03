use super::{ApiError, ApiState, auth::authorize};
use crate::domain::history::HistoryRequest;
use actix_web::{HttpRequest, HttpResponse, get, web};

fn parse(
    req: &HttpRequest,
    is_events: bool,
) -> Result<(HistoryRequest, Option<i64>, usize), ApiError> {
    let mut q = super::routes::parse_unique_query(req.query_string())?;
    let mut number = |key: &str, default: Option<i64>| -> Result<i64, ApiError> {
        q.remove(key)
            .map(|v| {
                v.parse::<i64>()
                    .map_err(|_| ApiError::BadRequest(format!("invalid {key}")))
            })
            .transpose()?
            .or(default)
            .ok_or_else(|| ApiError::BadRequest(format!("missing {key}")))
    };
    let from_ms = number("from", None)?;
    let to_ms = number("to", None)?;
    let max_points = number("max_points", Some(600))?;
    let before = if is_events {
        Some(number("before", Some(i64::MAX))?)
    } else {
        None
    };
    let limit = if is_events {
        number("limit", Some(100))?
    } else {
        100
    };
    if !(10..=1000).contains(&max_points)
        || !(1..=1000).contains(&limit)
        || before.is_some_and(|v| v <= 0)
    {
        return Err(ApiError::BadRequest("invalid point limit or cursor".into()));
    }
    let request = HistoryRequest {
        from_ms,
        to_ms,
        max_points: max_points as usize,
        module: q.remove("module").unwrap_or_else(|| "icmp".into()),
        check: q.remove("check"),
        hosts: q
            .remove("hosts")
            .map(|v| v.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        groups: q
            .remove("groups")
            .map(|v| v.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        breakdown: q.remove("breakdown").unwrap_or_else(|| "all".into()),
    };
    if !q.is_empty() {
        return Err(ApiError::BadRequest("unsupported history parameter".into()));
    }
    request
        .validate(chrono::Utc::now().timestamp_millis())
        .map_err(|e| ApiError::BadRequest(e.into()))?;
    Ok((request, before, limit as usize))
}

#[utoipa::path(get,path="/v1/history",params(
    ("from"=i64,Query,description="Inclusive Unix milliseconds; rounded down to source resolution"),
    ("to"=i64,Query,description="Exclusive Unix milliseconds; rounded up to source resolution, capped at now"),
    ("module"=Option<String>,Query,description="icmp (default), tcp, usage"),
    ("check"=Option<String>,Query,description="TCP check identifier"),
    ("hosts"=Option<String>,Query,description="Comma-separated host IDs; up to 16"),
    ("groups"=Option<String>,Query,description="Comma-separated group IDs; union scope using historical membership"),
    ("breakdown"=Option<String>,Query,description="all (default), group, host; up to 16 series"),
    ("max_points"=Option<usize>,Query,description="10..1000 points per series; default 600")
),responses((status=200,body=crate::domain::history::HistoryResponse),(status=400,description="Invalid query"),(status=401,description="Unauthorized"),(status=503,description="Query capacity exceeded")))]
#[get("/v1/history")]
pub(crate) async fn series(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<HttpResponse, ApiError> {
    authorize(&req, &state)?;
    let (query, _, _) = parse(&req, false)?;
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(state.history.history_series(query).await?))
}

#[utoipa::path(get,path="/v1/history/events",responses((status=200,body=crate::domain::history::HistoryEventsResponse),(status=400,description="Invalid query"),(status=401,description="Unauthorized"),(status=503,description="Query capacity exceeded")))]
#[get("/v1/history/events")]
pub(crate) async fn events(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<HttpResponse, ApiError> {
    event_response(req, state, false).await
}
#[utoipa::path(get,path="/v1/history/samples",responses((status=200,body=crate::domain::history::HistoryEventsResponse),(status=400,description="Invalid query"),(status=401,description="Unauthorized"),(status=503,description="Query capacity exceeded")))]
#[get("/v1/history/samples")]
pub(crate) async fn samples(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<HttpResponse, ApiError> {
    event_response(req, state, true).await
}
async fn event_response(
    req: HttpRequest,
    state: web::Data<ApiState>,
    is_samples: bool,
) -> Result<HttpResponse, ApiError> {
    authorize(&req, &state)?;
    let (query, before, limit) = parse(&req, true)?;
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(
            state
                .history
                .history_events(query, before, limit, is_samples)
                .await?,
        ))
}
#[utoipa::path(get,path="/v1/system/maintenance",responses((status=200,body=crate::domain::maintenance::MaintenanceStatus),(status=401,description="Unauthorized")))]
#[get("/v1/system/maintenance")]
pub(crate) async fn maintenance(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<HttpResponse, ApiError> {
    authorize(&req, &state)?;
    if !req.query_string().is_empty() {
        return Err(ApiError::BadRequest(
            "maintenance does not accept query parameters".into(),
        ));
    }
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(state.history.maintenance_status().await?))
}
pub(super) fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(series)
        .service(events)
        .service(samples)
        .service(maintenance);
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, http::StatusCode, test};
    use std::sync::{Arc, RwLock};
    fn state() -> ApiState {
        let storage = Arc::new(crate::storage::SqliteStorage::in_memory(vec![]).unwrap());
        ApiState {
            metrics: Default::default(),
            hosts: storage.clone(),
            icmp: storage.clone(),
            usage: storage.clone(),
            history: storage,
            api_token: Arc::new(RwLock::new(Some(
                crate::domain::ApiToken::new("fake-test-token").unwrap(),
            ))),
            module_config: Default::default(),
        }
    }
    #[actix_web::test]
    async fn new_routes_require_authentication() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state()))
                .configure(configure),
        )
        .await;
        for path in [
            "/v1/history",
            "/v1/history/events",
            "/v1/history/samples",
            "/v1/system/maintenance",
        ] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }
    #[actix_web::test]
    async fn bounded_queries_reject_duplicate_unknown_and_invalid_parameters() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state()))
                .configure(configure),
        )
        .await;
        for query in [
            "from=1&to=10&from=2",
            "from=1&to=10&unexpected=1",
            "from=10&to=1",
            "from=1&to=10&max_points=1001",
            "from=1&to=10&module=ssh",
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(&format!("/v1/history?{query}"))
                    .insert_header(("Authorization", "Bearer fake-test-token"))
                    .to_request(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
    }
    #[actix_web::test]
    async fn returns_empty_history_without_fabricating_observations() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state()))
                .configure(configure),
        )
        .await;
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/v1/history?from=1&to=10000")
                .insert_header(("Authorization", "Bearer fake-test-token"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["series"], serde_json::json!([]));
    }
}
