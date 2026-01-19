use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Database instance not found")]
    DbNotFound,

    #[error("Unsupported dialect: {0}")]
    DialectUnsupported(String),

    #[error("Failed to pull Docker image: {0}")]
    DialectPullFailed(String),

    #[error("Query exceeded timeout limit")]
    QueryTimeout,

    #[error("SQL syntax error: {0}")]
    QuerySyntaxError(String),

    #[error("Database exceeded size limit")]
    DbSizeExceeded,

    #[error("Backup not found")]
    BackupNotFound,

    #[error("Backup has expired")]
    BackupExpired,

    #[error("Docker error: {0}")]
    Docker(#[from] bollard::errors::Error),

    #[error("Internal server error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::DbNotFound => "DB_NOT_FOUND",
            Self::DialectUnsupported(_) => "DIALECT_UNSUPPORTED",
            Self::DialectPullFailed(_) => "DIALECT_PULL_FAILED",
            Self::QueryTimeout => "QUERY_TIMEOUT",
            Self::QuerySyntaxError(_) => "QUERY_SYNTAX_ERROR",
            Self::DbSizeExceeded => "DB_SIZE_EXCEEDED",
            Self::BackupNotFound => "BACKUP_NOT_FOUND",
            Self::BackupExpired => "BACKUP_EXPIRED",
            Self::Docker(_) => "DOCKER_ERROR",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::DbNotFound => StatusCode::NOT_FOUND,
            Self::DialectUnsupported(_) => StatusCode::BAD_REQUEST,
            Self::DialectPullFailed(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::QueryTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::QuerySyntaxError(_) => StatusCode::BAD_REQUEST,
            Self::DbSizeExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::BackupNotFound => StatusCode::NOT_FOUND,
            Self::BackupExpired => StatusCode::GONE,
            Self::Docker(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let detail = match &self {
            Self::QuerySyntaxError(msg) => Some(msg.clone()),
            Self::DialectPullFailed(msg) => Some(msg.clone()),
            Self::Docker(e) => Some(e.to_string()),
            Self::Internal(msg) => Some(msg.clone()),
            _ => None,
        };

        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code(),
                message: self.to_string(),
                detail,
            },
        };

        (self.status_code(), Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
