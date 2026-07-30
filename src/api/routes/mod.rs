use crate::{
    api::{
        ApiError, ApiState,
        auth::authorize,
        dto::{
            HealthResponse, HostResponse, IcmpTransitionResponse, ModuleResponse, ModulesResponse,
            NamespaceCatalogResponse, UsageHistoryResponse, UsageReportResponse,
            UsageSampleResponse,
        },
    },
    app::filters::{
        namespace_catalog, parse_host_filter, parse_usage_filter, supported_module_filter_specs,
    },
    modules,
};
use actix_web::{HttpRequest, HttpResponse, Responder, get, web};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/healthz",
    responses((status = 200, description = "Service is healthy", body = HealthResponse))
)]
#[get("/healthz")]
pub(crate) async fn healthz() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse { status: "ok" })
}

#[utoipa::path(
    get,
    path = "/v1/hosts",
    params(
        ("group" = Option<String>, Query, description = "Host group id"),
        ("icmp.status" = Option<String>, Query, description = "ICMP status: unknown, up, or down"),
        ("metadata.*" = Option<String>, Query, description = "Host metadata exact match, such as metadata.room=net-a"),
        ("usage.status" = Option<String>, Query, description = "Usage collection status: ok or error"),
        ("usage.inactive_console_for" = Option<String>, Query, description = "Duration such as 24h or 7days"),
        ("usage.no_users_for" = Option<String>, Query, description = "Duration such as 24h or 7days")
    ),
    responses(
        (status = 200, description = "Matching hosts", body = Vec<HostResponse>),
        (status = 400, description = "Invalid filter", body = crate::api::dto::ErrorResponse),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/hosts")]
pub(crate) async fn all_hosts(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let filter = parse_host_filter(parse_unique_query(req.query_string())?)?;
    let hosts: Vec<HostResponse> = state
        .hosts
        .hosts(filter)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(HttpResponse::Ok().json(hosts))
}

#[utoipa::path(
    get,
    path = "/v1/hosts/{id}",
    params(("id" = String, Path, description = "Host id")),
    responses(
        (status = 200, description = "Host", body = HostResponse),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse),
        (status = 404, description = "Host not found", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/hosts/{id}")]
pub(crate) async fn host(
    req: HttpRequest,
    state: web::Data<ApiState>,
    id: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let id = id.into_inner();
    match state.hosts.host(&id).await? {
        Some(host) => Ok(HttpResponse::Ok().json(HostResponse::from(host))),
        None => Err(ApiError::NotFound(id)),
    }
}

#[utoipa::path(
    get,
    path = "/v1/usage",
    params(
        ("usage.status" = Option<String>, Query, description = "Usage collection status: ok or error"),
        ("usage.inactive_console_for" = Option<String>, Query, description = "Duration such as 24h or 7days"),
        ("usage.no_users_for" = Option<String>, Query, description = "Duration such as 24h or 7days")
    ),
    responses(
        (status = 200, description = "Aggregate usage report", body = UsageReportResponse),
        (status = 400, description = "Invalid filter", body = crate::api::dto::ErrorResponse),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/usage")]
pub(crate) async fn usage_summary(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let filter = parse_usage_filter(parse_unique_query(req.query_string())?)?;
    let report = UsageReportResponse::from(state.usage.usage_report(filter).await?);
    Ok(HttpResponse::Ok().json(report))
}

#[utoipa::path(
    get,
    path = "/v1/namespaces",
    responses(
        (status = 200, description = "Filter namespace catalog", body = NamespaceCatalogResponse),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/namespaces")]
pub(crate) async fn namespaces(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    Ok(HttpResponse::Ok().json(NamespaceCatalogResponse::from(namespace_catalog())))
}

#[utoipa::path(
    get,
    path = "/v1/modules",
    responses(
        (status = 200, description = "Monitor module catalog", body = ModulesResponse),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/modules")]
pub(crate) async fn module_catalog(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let module_config = state
        .module_config
        .read()
        .map_err(|_| ApiError::Internal("module config lock poisoned".into()))?;
    let modules = modules::registry()
        .iter()
        .map(|module| {
            let metadata = module.metadata();
            let enabled = match metadata.id {
                "icmp" => module_config.icmp.enabled,
                "usage" => module_config.usage.enabled,
                _ => false,
            };
            let filters = supported_module_filter_specs(metadata.id, module.filter_specs());
            ModuleResponse::new(metadata, enabled, filters, module.config_options())
        })
        .collect();
    Ok(HttpResponse::Ok().json(ModulesResponse { modules }))
}

/// Shared paging knobs for all per-host history/sample endpoints.
const HISTORY_LIMIT_DEFAULT: usize = 100;
const HISTORY_LIMIT_MAX: usize = 1000;
const HISTORY_LIMIT_DOC: &str = "Maximum rows to return, clamped to 1..1000";

fn history_limit(query: &HistoryQuery) -> usize {
    query
        .limit
        .unwrap_or(HISTORY_LIMIT_DEFAULT)
        .clamp(1, HISTORY_LIMIT_MAX)
}

fn parse_unique_query(query: &str) -> Result<HashMap<String, String>, ApiError> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(query)
        .map_err(|err| ApiError::BadRequest(format!("invalid query string: {err}")))?;
    let mut seen = HashSet::new();
    let mut params = HashMap::with_capacity(pairs.len());
    for (key, value) in pairs {
        if !seen.insert(key.clone()) {
            return Err(ApiError::BadRequest(format!(
                "duplicate query parameter {key:?}"
            )));
        }
        params.insert(key, value);
    }
    Ok(params)
}

#[utoipa::path(
    get,
    path = "/v1/hosts/{id}/history",
    params(
        ("id" = String, Path, description = "Host id"),
        ("limit" = Option<usize>, Query, description = HISTORY_LIMIT_DOC)
    ),
    responses(
        (status = 200, description = "ICMP transition history", body = Vec<IcmpTransitionResponse>),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse),
        (status = 404, description = "Host not found", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/hosts/{id}/history")]
pub(crate) async fn host_history(
    req: HttpRequest,
    state: web::Data<ApiState>,
    id: web::Path<String>,
    query: web::Query<HistoryQuery>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let limit = history_limit(&query);
    let history: Vec<IcmpTransitionResponse> = state
        .icmp
        .history(&id, limit)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(HttpResponse::Ok().json(history))
}

#[utoipa::path(
    get,
    path = "/v1/hosts/{id}/usage/history",
    params(
        ("id" = String, Path, description = "Host id"),
        ("limit" = Option<usize>, Query, description = HISTORY_LIMIT_DOC)
    ),
    responses(
        (status = 200, description = "Usage change history", body = Vec<UsageHistoryResponse>),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse),
        (status = 404, description = "Host not found", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/hosts/{id}/usage/history")]
pub(crate) async fn host_usage_history(
    req: HttpRequest,
    state: web::Data<ApiState>,
    id: web::Path<String>,
    query: web::Query<HistoryQuery>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let limit = history_limit(&query);
    let history: Vec<UsageHistoryResponse> = state
        .usage
        .usage_history(&id, limit)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(HttpResponse::Ok().json(history))
}

#[utoipa::path(
    get,
    path = "/v1/hosts/{id}/usage/samples",
    params(
        ("id" = String, Path, description = "Host id"),
        ("limit" = Option<usize>, Query, description = HISTORY_LIMIT_DOC)
    ),
    responses(
        (status = 200, description = "Usage sample history", body = Vec<UsageSampleResponse>),
        (status = 401, description = "Unauthorized", body = crate::api::dto::ErrorResponse),
        (status = 404, description = "Host not found", body = crate::api::dto::ErrorResponse)
    )
)]
#[get("/v1/hosts/{id}/usage/samples")]
pub(crate) async fn host_usage_samples(
    req: HttpRequest,
    state: web::Data<ApiState>,
    id: web::Path<String>,
    query: web::Query<HistoryQuery>,
) -> Result<impl Responder, ApiError> {
    authorize(&req, &state)?;
    let limit = history_limit(&query);
    let samples: Vec<UsageSampleResponse> = state
        .usage
        .usage_samples(&id, limit)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(HttpResponse::Ok().json(samples))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(healthz)
        .service(namespaces)
        .service(module_catalog)
        .service(all_hosts)
        .service(host)
        .configure(|cfg| {
            for module in modules::registry() {
                module.register_routes(cfg);
            }
        });
}
