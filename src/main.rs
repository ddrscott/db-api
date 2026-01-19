mod api;
mod config;
mod db;
mod docker;
mod error;
mod storage;

use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::api::create_router;
use crate::config::Config;
use crate::db::InstanceManager;
use crate::docker::DockerManager;
use crate::storage::{BackupManager, MetadataStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "db_api=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    let config = Config::from_env();
    info!("Configuration loaded: {:?}", config);

    // Initialize Docker manager
    let docker = DockerManager::new().expect("Failed to connect to Docker");
    let docker = Arc::new(docker);

    // Check Docker connectivity
    docker
        .health_check()
        .await
        .expect("Failed to connect to Docker daemon");
    info!("Connected to Docker daemon");

    // Initialize metadata store (SQLite)
    let metadata = MetadataStore::new(&config.metadata_db_path)
        .expect("Failed to initialize metadata store");
    info!("Metadata store initialized at {}", config.metadata_db_path);

    // Initialize backup manager (R2) if configured
    let backup = if config.backup_enabled() {
        match BackupManager::new(&config).await {
            Ok(b) => {
                info!("Backup manager initialized for bucket {}", config.r2_bucket);
                Some(b)
            }
            Err(e) => {
                tracing::warn!("Failed to initialize backup manager: {}. Backups disabled.", e);
                None
            }
        }
    } else {
        info!("Backup not configured or disabled");
        None
    };

    // Initialize instance manager
    let manager = InstanceManager::new(
        DockerManager::new().expect("Failed to create Docker manager"),
        metadata,
        backup,
        config.clone(),
    );
    let manager = Arc::new(manager);

    // Recover existing instances from Docker containers
    match manager.recover_existing_instances().await {
        Ok(count) if count > 0 => info!("Recovered {} existing database instance(s)", count),
        Ok(_) => info!("No existing database instances to recover"),
        Err(e) => tracing::warn!("Failed to recover existing instances: {}", e),
    }

    // Start cleanup task
    manager.clone().start_cleanup_task();
    info!("Started instance cleanup task");

    // Create router
    let app = create_router(manager, docker, &config);

    // Start server
    let addr = config.socket_addr();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
