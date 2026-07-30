use crate::{
    api::{dto, routes},
    app::{FilterKeyMetadata, filters::namespace_catalog},
};
use actix_web::{HttpResponse, get, web};
use serde_json::{Value, json};
use utoipa::OpenApi;
use utoipa_swagger_ui::{Config, SwaggerUi};

#[derive(OpenApi)]
#[openapi(
    paths(
        routes::healthz,
        routes::all_hosts,
        routes::host,
        routes::usage_summary,
        routes::namespaces,
        routes::module_catalog,
        routes::host_history,
        routes::host_usage_history,
        routes::host_usage_samples
    ),
    components(schemas(
        dto::ErrorResponse,
        dto::HealthResponse,
        dto::NamespaceCatalogResponse,
        dto::FilterKeyResponse,
        dto::ModulesResponse,
        dto::ModuleResponse,
        dto::ConfigOptionResponse,
        dto::HostResponse,
        dto::IcmpTransitionResponse,
        dto::UsageHistoryResponse,
        dto::UsageSampleResponse,
        dto::UsageReportResponse
    )),
    tags((name = "simple-network-monitor", description = "ICMP and usage monitoring API"))
)]
struct ApiDoc;

#[get("/openapi.json")]
async fn openapi_json() -> HttpResponse {
    let mut doc = serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI document serializes");
    inject_filter_params(&mut doc);
    HttpResponse::Ok().json(doc)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(openapi_json)
        .service(SwaggerUi::new("/swagger-ui/{_:.*}").config(Config::from("/openapi.json")));
}

fn inject_filter_params(doc: &mut Value) {
    let catalog = namespace_catalog();
    let mut host_params = Vec::new();
    for (key, spec) in catalog.bare {
        host_params.push(query_param(key, spec));
    }
    for (namespace, keys) in &catalog.namespaces {
        for (key, spec) in keys {
            host_params.push(query_param(&format!("{namespace}.{key}"), spec.clone()));
        }
    }
    replace_query_params(doc, "/v1/hosts", host_params);

    let usage_params = catalog
        .namespaces
        .get("usage")
        .into_iter()
        .flat_map(|keys| keys.iter())
        .map(|(key, spec)| query_param(&format!("usage.{key}"), spec.clone()))
        .collect();
    replace_query_params(doc, "/v1/usage", usage_params);
}

fn replace_query_params(doc: &mut Value, path: &str, query_params: Vec<Value>) {
    let Some(parameters) = doc
        .pointer_mut(&format!(
            "/paths/{}/get/parameters",
            path.replace('/', "~1")
        ))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    parameters.retain(|param| param.get("in").and_then(Value::as_str) != Some("query"));
    parameters.extend(query_params);
}

fn query_param(name: &str, spec: FilterKeyMetadata) -> Value {
    let mut schema = json!({ "type": "string" });
    if !spec.values.is_empty() {
        schema["enum"] = json!(spec.values);
    }
    json!({
        "name": name,
        "in": "query",
        "required": false,
        "schema": schema,
        "description": spec.description
    })
}
