use crate::{
    api::{ApiError, ApiState},
    storage::StorageError,
};
use actix_web::{HttpRequest, http::header};
use subtle::ConstantTimeEq;

const BEARER_PREFIX: &str = "Bearer ";

pub fn authorize(req: &HttpRequest, state: &ApiState) -> Result<(), ApiError> {
    let token = state
        .api_token
        .read()
        .map_err(|_| ApiError::Storage(StorageError::LockPoisoned))?;
    let Some(expected) = token.as_ref() else {
        return Ok(());
    };
    let Some(value) = req.headers().get(header::AUTHORIZATION) else {
        return Err(ApiError::Unauthorized);
    };
    let Ok(value) = value.to_str() else {
        return Err(ApiError::Unauthorized);
    };
    let Some(presented) = value.strip_prefix(BEARER_PREFIX) else {
        return Err(ApiError::Unauthorized);
    };
    if presented.as_bytes().ct_eq(expected.expose_bytes()).into() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiState;
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    fn state(token: Option<&str>) -> ApiState {
        let storage = Arc::new(crate::storage::SqliteStorage::in_memory(vec![]).unwrap());
        ApiState {
            hosts: storage.clone(),
            icmp: storage.clone(),
            usage: storage,
            api_token: Arc::new(std::sync::RwLock::new(
                token.map(crate::domain::ApiToken::from),
            )),
            module_config: Arc::new(std::sync::RwLock::new(
                crate::config::ModuleConfigs::default(),
            )),
        }
    }

    #[test]
    fn accepts_correct_bearer() {
        let req = TestRequest::default()
            .insert_header(("authorization", "Bearer secret"))
            .to_http_request();
        assert!(authorize(&req, &state(Some("secret"))).is_ok());
    }

    #[test]
    fn rejects_wrong_bearer() {
        let req = TestRequest::default()
            .insert_header(("authorization", "Bearer wrong"))
            .to_http_request();
        assert!(authorize(&req, &state(Some("secret"))).is_err());
    }

    #[test]
    fn rejects_length_mismatch_without_panic() {
        let req = TestRequest::default()
            .insert_header(("authorization", "Bearer x"))
            .to_http_request();
        assert!(authorize(&req, &state(Some("longer-token"))).is_err());
    }

    #[test]
    fn rejects_missing_bearer_prefix() {
        let req = TestRequest::default()
            .insert_header(("authorization", "Basic secret"))
            .to_http_request();
        assert!(authorize(&req, &state(Some("secret"))).is_err());
    }

    #[test]
    fn allows_when_no_token_configured() {
        let req = TestRequest::default().to_http_request();
        assert!(authorize(&req, &state(None)).is_ok());
    }
}
