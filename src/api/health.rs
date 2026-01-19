use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::docker::DockerManager;
use crate::error::Result;

pub struct HealthState {
    pub docker: Arc<DockerManager>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub docker: &'static str,
}

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub active_instances: usize,
}

pub async fn health_check(State(state): State<Arc<HealthState>>) -> Result<Json<HealthResponse>> {
    let docker_status = match state.docker.health_check().await {
        Ok(true) => "connected",
        _ => "disconnected",
    };

    let status = if docker_status == "connected" {
        "healthy"
    } else {
        "unhealthy"
    };

    Ok(Json(HealthResponse {
        status,
        docker: docker_status,
    }))
}
