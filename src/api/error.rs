//! Shared JSON error responses for HTTP handlers and routing fallbacks.
//!
//! Handlers may return `Result<T, ApiError>` and convert unexpected failures
//! from `anyhow::Error` with `?`. Internal causes are logged only when the
//! error becomes an HTTP response; they never enter the serialized envelope.
//!
//! Responses use `application/json` and an `error` object containing `code`
//! and `message`. Codes are stable identifiers for clients:
//!
//! - 400: `bad_request`, malformed JSON or invalid query/path parameters.
//! - 413: `payload_too_large`, request body exceeds the extractor limit.
//! - 415: `unsupported_media_type`, JSON content type is required.
//! - 422: `validation_error`, invalid JSON fields or domain input.
//! - 404: `not_found`, "Resource not found".
//! - 405: `method_not_allowed`, "Method not allowed"; routing adds `Allow`.
//! - 500: `internal_error`, "Internal server error".

use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

/// HTTP failure with a stable client-facing code and message.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    /// Malformed request parameters or JSON syntax.
    #[error("{0}")]
    BadRequest(&'static str),
    /// JSON input cannot be deserialized or fails domain validation.
    #[error("{0}")]
    Validation(&'static str),
    /// The request body exceeds the JSON extractor's size limit.
    #[error("Request body is too large")]
    PayloadTooLarge,
    /// JSON operations require an application/json content type.
    #[error("Content-Type must be application/json")]
    UnsupportedMediaType,
    /// No resource or route matches the request.
    #[error("Resource not found")]
    NotFound,
    /// The route exists but does not accept the requested HTTP method.
    #[error("Method not allowed")]
    MethodNotAllowed,
    /// Unexpected failure whose source is reserved for server logs.
    #[error("Internal server error")]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ApiError {
    /// Preserve database diagnostics for server logs only.
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl From<JsonRejection> for ApiError {
    /// Preserve rejection statuses with safe messages and the shared envelope.
    fn from(error: JsonRejection) -> Self {
        match error.status() {
            StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::UnsupportedMediaType,
            StatusCode::UNPROCESSABLE_ENTITY => {
                Self::Validation("Invalid JSON fields")
            }
            _ => Self::BadRequest("Invalid JSON body"),
        }
    }
}

/// Top-level envelope shared by API error responses.
#[derive(Serialize, ToSchema, utoipa::ToResponse)]
pub(super) struct ErrorResponse {
    /// Public error details, without internal failure causes.
    error: ErrorBody,
}

/// Stable machine-readable code and human-readable explanation.
#[derive(Serialize, ToSchema)]
struct ErrorBody {
    /// Stable lowercase identifier for client-side error handling.
    #[schema(examples(
        "bad_request",
        "payload_too_large",
        "unsupported_media_type",
        "validation_error",
        "not_found",
        "method_not_allowed",
        "internal_error"
    ))]
    code: &'static str,
    /// Safe explanation intended for end users.
    #[schema(example = "Resource not found")]
    message: String,
}

impl IntoResponse for ApiError {
    /// Render the JSON envelope and log unexpected failures once at ERROR.
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Validation(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "validation_error")
            }
            Self::PayloadTooLarge => {
                (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
            }
            Self::UnsupportedMediaType => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
            }
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::MethodNotAllowed => {
                (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
            }
            Self::Internal(error) => {
                tracing::error!(
                    error = %format!("{error:#}"),
                    "HTTP request failed"
                );
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        let body = ErrorResponse {
            error: ErrorBody {
                code,
                message: self.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    //! Internal error diagnostics and client response isolation.

    use std::io::{Read, Seek, SeekFrom};

    use axum::body::to_bytes;

    use super::*;

    /// Logs include the full cause chain while JSON reveals no internal data.
    #[tokio::test]
    async fn internal_details_are_logged_but_not_returned() {
        let mut logs = tempfile::tempfile().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(logs.try_clone().unwrap())
            .finish();
        let cause = anyhow::anyhow!("database password=secret")
            .context("Failed to query library");
        let response = tracing::subscriber::with_default(subscriber, || {
            ApiError::from(cause).into_response()
        });

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            body.as_ref(),
            concat!(
                r#"{"error":{"code":"internal_error","#,
                r#""message":"Internal server error"}}"#
            )
            .as_bytes()
        );
        logs.seek(SeekFrom::Start(0)).unwrap();
        let mut output = String::new();
        logs.read_to_string(&mut output).unwrap();
        assert!(output.contains("ERROR"));
        assert!(output.contains("Failed to query library"));
        assert!(output.contains("database password=secret"));
        assert_eq!(output.matches("HTTP request failed").count(), 1);
    }
}
