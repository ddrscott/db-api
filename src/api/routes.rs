use axum::{
    response::Html,
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;

use crate::config::Config;
use crate::db::manager::InstanceManager;
use crate::db::query::QueryExecutor;
use crate::docker::DockerManager;

use super::db::{create_db, destroy_db, execute_query, get_db_status, AppState};
use super::health::{health_check, HealthState};
use super::openapi::{openapi_spec, swagger_ui};

async fn get_openapi() -> &'static str {
    openapi_spec()
}

async fn get_docs() -> Html<&'static str> {
    swagger_ui()
}

pub fn create_router(
    manager: Arc<InstanceManager>,
    docker: Arc<DockerManager>,
    config: &Config,
) -> Router {
    let app_state = Arc::new(AppState {
        manager: manager.clone(),
        query_executor: QueryExecutor::new(docker.clone(), config.query_timeout),
        inactivity_timeout_secs: config.inactivity_timeout.as_secs() as i64,
    });

    let health_state = Arc::new(HealthState { docker });

    let db_routes = Router::new()
        .route("/new", post(create_db))
        .route("/{db_id}", get(get_db_status))
        .route("/{db_id}", delete(destroy_db))
        .route("/{db_id}/query", post(execute_query))
        .with_state(app_state);

    let health_routes = Router::new()
        .route("/health", get(health_check))
        .with_state(health_state);

    Router::new()
        .nest("/db", db_routes)
        .merge(health_routes)
        .route("/openapi.json", get(get_openapi))
        .route("/docs", get(get_docs))
}
