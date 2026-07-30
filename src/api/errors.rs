use crate::{api::dto::ErrorResponse, app::FilterParseError, storage::StorageError};
use actix_web::{HttpResponse, ResponseError, http::StatusCode};
use std::fmt;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    BadRequest(String),
    NotFound(String),
    Internal(String),
    Storage(StorageError),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::BadRequest(value) => write!(f, "bad request: {value}"),
            Self::NotFound(value) => write!(f, "{value} not found"),
            Self::Internal(value) => write!(f, "internal error: {value}"),
            Self::Storage(err) => write!(f, "{err}"),
        }
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) | Self::Storage(StorageError::NotFound(_)) => StatusCode::NOT_FOUND,
            Self::Internal(_) | Self::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code()).json(ErrorResponse {
            error: self.to_string(),
        })
    }
}

impl From<StorageError> for ApiError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<FilterParseError> for ApiError {
    fn from(value: FilterParseError) -> Self {
        Self::BadRequest(value.to_string())
    }
}
