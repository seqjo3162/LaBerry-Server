use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized(&'static str),
    Forbidden(&'static str),
    NotFound(&'static str),
    TooManyRequests(&'static str),
    Internal(&'static str),
}

#[derive(Serialize)]
struct ErrorBody {
    detail: &'static str,
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            ApiError::BadRequest(m)
            | ApiError::Unauthorized(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::TooManyRequests(m)
            | ApiError::Internal(m) => m,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status_code(),
            Json(ErrorBody {
                detail: self.message(),
            }),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(_: sqlx::Error) -> Self {
        ApiError::Internal("Database error")
    }
}
