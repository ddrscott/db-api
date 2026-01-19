use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::db::instance::InstanceStatus;
use crate::db::manager::InstanceManager;
use crate::db::query::QueryExecutor;
use crate::error::Result;

use super::response::{create_json_response, create_sse_response, create_text_response};

#[derive(Debug, Deserialize)]
pub struct CreateDbRequest {
    pub dialect: String,
    /// Optional db_id to restore an existing archived database
    #[serde(default)]
    pub db_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct CreateDbResponse {
    pub db_id: Uuid,
    pub dialect: String,
    pub status: InstanceStatus,
    /// True if database was restored from backup
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
}

#[derive(Debug, Serialize)]
pub struct DbStatusResponse {
    pub db_id: Uuid,
    pub dialect: String,
    pub status: InstanceStatus,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// True if a backup exists for this database
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub backup_available: bool,
    /// When the database was archived (if archived)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct DestroyDbResponse {
    pub db_id: Uuid,
    pub status: InstanceStatus,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub query: String,
    /// Output format: "text", "json", "jsonl" (default: "json")
    #[serde(default)]
    pub format: Option<String>,
    /// Transport: "sse" (default: none, except jsonl implies sse)
    #[serde(default)]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

impl QueryRequest {
    fn resolve_format(&self) -> OutputFormat {
        match (self.format.as_deref(), self.transport.as_deref()) {
            // Explicit format=text
            (Some("text"), _) => OutputFormat::Text,
            // Explicit format=json
            (Some("json"), _) => OutputFormat::Json,
            // Explicit format=jsonl (implies SSE transport)
            (Some("jsonl"), _) => OutputFormat::Jsonl,
            // Explicit transport=sse (implies jsonl format)
            (None, Some("sse")) => OutputFormat::Jsonl,
            // No params: default to json
            (None, None) => OutputFormat::Json,
            // Unknown format: default to json
            _ => OutputFormat::Json,
        }
    }
}

pub struct AppState {
    pub manager: Arc<InstanceManager>,
    pub query_executor: QueryExecutor,
    pub inactivity_timeout_secs: i64,
}

pub async fn create_db(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateDbRequest>,
) -> Result<Json<CreateDbResponse>> {
    let (instance, restored) = state
        .manager
        .get_or_create_instance(&req.dialect, req.db_id)
        .await?;

    Ok(Json(CreateDbResponse {
        db_id: instance.id,
        dialect: instance.dialect,
        status: instance.status,
        restored,
    }))
}

pub async fn get_db_status(
    State(state): State<Arc<AppState>>,
    Path(db_id): Path<Uuid>,
) -> Result<Json<DbStatusResponse>> {
    // Try to get active instance first
    match state.manager.get_instance(db_id).await {
        Ok(instance) => {
            // Check metadata for backup info
            let stored = state.manager.get_stored_instance(db_id)?;
            let backup_available = stored.as_ref().map(|s| s.backup_key.is_some()).unwrap_or(false);

            let expires_at = instance.last_activity
                + chrono::Duration::seconds(state.inactivity_timeout_secs);

            Ok(Json(DbStatusResponse {
                db_id: instance.id,
                dialect: instance.dialect,
                status: instance.status,
                created_at: instance.created_at,
                last_activity: instance.last_activity,
                expires_at,
                backup_available,
                archived_at: None,
            }))
        }
        Err(crate::error::AppError::DbNotFound) => {
            // Check if archived
            if let Some(stored) = state.manager.get_stored_instance(db_id)? {
                let expires_at = stored.last_activity
                    + chrono::Duration::seconds(state.inactivity_timeout_secs);

                Ok(Json(DbStatusResponse {
                    db_id: stored.db_id,
                    dialect: stored.dialect,
                    status: InstanceStatus::Archived,
                    created_at: stored.created_at,
                    last_activity: stored.last_activity,
                    expires_at,
                    backup_available: stored.backup_key.is_some(),
                    archived_at: stored.archived_at,
                }))
            } else {
                Err(crate::error::AppError::DbNotFound)
            }
        }
        Err(e) => Err(e),
    }
}

pub async fn destroy_db(
    State(state): State<Arc<AppState>>,
    Path(db_id): Path<Uuid>,
) -> Result<Json<DestroyDbResponse>> {
    state.manager.destroy_instance(db_id).await?;

    Ok(Json(DestroyDbResponse {
        db_id,
        status: InstanceStatus::Destroyed,
    }))
}

pub async fn execute_query(
    State(state): State<Arc<AppState>>,
    Path(db_id): Path<Uuid>,
    Json(req): Json<QueryRequest>,
) -> Result<Response> {
    // Touch the instance to update last activity
    state.manager.touch_instance(db_id).await?;

    let instance = state.manager.get_instance(db_id).await?;
    let format = req.resolve_format();

    match format {
        OutputFormat::Text => {
            // Return raw CLI output
            let output = state.query_executor.execute_raw(&instance, &req.query).await?;
            Ok(create_text_response(output))
        }
        OutputFormat::Json => {
            // Return traditional JSON array
            let stream = state.query_executor.execute(&instance, &req.query).await?;
            let events: Vec<_> = stream.collect().await;
            Ok(create_json_response(events).into_response())
        }
        OutputFormat::Jsonl => {
            // Return SSE stream with JSONL events
            let stream = state.query_executor.execute(&instance, &req.query).await?;
            Ok(create_sse_response(stream).into_response())
        }
    }
}
