use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::docker::DockerManager;
use crate::error::{AppError, Result};
use crate::storage::{BackupManager, InstanceState, MetadataStore, StoredInstance};

use super::dialects::{get_dialect, Dialect};
use super::instance::{DbInstance, InstanceStatus};

pub struct InstanceManager {
    /// In-memory cache for active instances (fast access)
    instances: Arc<RwLock<HashMap<Uuid, DbInstance>>>,
    /// Persistent metadata store (SQLite)
    metadata: Arc<MetadataStore>,
    /// Optional backup manager (R2)
    backup: Option<Arc<BackupManager>>,
    docker: Arc<DockerManager>,
    config: Config,
}

impl InstanceManager {
    pub fn new(
        docker: DockerManager,
        metadata: MetadataStore,
        backup: Option<BackupManager>,
        config: Config,
    ) -> Self {
        Self {
            instances: Arc::new(RwLock::new(HashMap::new())),
            metadata: Arc::new(metadata),
            backup: backup.map(Arc::new),
            docker: Arc::new(docker),
            config,
        }
    }

    pub fn docker(&self) -> Arc<DockerManager> {
        self.docker.clone()
    }

    /// Create a new database instance
    pub async fn create_instance(&self, dialect_name: &str) -> Result<DbInstance> {
        self.create_instance_with_id(dialect_name, None).await
    }

    /// Create or restore a database instance
    /// If db_id is provided and exists as archived, restore it
    pub async fn get_or_create_instance(
        &self,
        dialect_name: &str,
        db_id: Option<Uuid>,
    ) -> Result<(DbInstance, bool)> {
        // Check if we should restore an existing instance
        if let Some(id) = db_id {
            // Check metadata store for this ID
            if let Some(stored) = self.metadata.get_instance(id)? {
                match stored.status {
                    InstanceState::Active => {
                        // Already active, return from cache
                        let instances = self.instances.read().await;
                        if let Some(instance) = instances.get(&id) {
                            return Ok((instance.clone(), false));
                        }
                        // Not in cache but marked active - inconsistent state, try to recover
                        drop(instances);
                        return self.recover_single_instance(&stored).await.map(|i| (i, false));
                    }
                    InstanceState::Archived => {
                        // Restore from backup
                        let instance = self.restore_instance(&stored).await?;
                        return Ok((instance, true));
                    }
                    InstanceState::Restoring => {
                        // Already being restored
                        return Err(AppError::RestoreInProgress);
                    }
                }
            }
            // ID provided but not found - create new with this ID
            let instance = self.create_instance_with_id(dialect_name, Some(id)).await?;
            return Ok((instance, false));
        }

        // No ID provided - create new
        let instance = self.create_instance_with_id(dialect_name, None).await?;
        Ok((instance, false))
    }

    async fn create_instance_with_id(
        &self,
        dialect_name: &str,
        specified_id: Option<Uuid>,
    ) -> Result<DbInstance> {
        let dialect = get_dialect(dialect_name)?;
        let id = specified_id.unwrap_or_else(Uuid::new_v4);

        // Generate unique credentials for this instance
        let db_name = format!("db_{}", id.simple());
        let db_user = format!("user_{}", &id.simple().to_string()[..8]);
        let db_password = generate_password();

        let env_vars = dialect.env_vars(&db_name, &db_user, &db_password);

        // Store metadata in container labels for recovery after restart
        let mut labels = HashMap::new();
        labels.insert("db-api.id".to_string(), id.to_string());
        labels.insert("db-api.dialect".to_string(), dialect_name.to_string());
        labels.insert("db-api.db_name".to_string(), db_name.clone());
        labels.insert("db-api.db_user".to_string(), db_user.clone());
        labels.insert("db-api.db_password".to_string(), db_password.clone());
        labels.insert(
            "db-api.container_port".to_string(),
            dialect.default_port().to_string(),
        );

        info!(
            "Creating {} instance {} with database {}",
            dialect_name, id, db_name
        );

        let (container_id, host_port) = self
            .docker
            .create_container(
                id,
                dialect.docker_image(),
                env_vars,
                dialect.default_port(),
                self.config.container_memory_mb,
                labels,
            )
            .await?;

        let mut instance = DbInstance::new(
            id,
            dialect_name.to_string(),
            container_id.clone(),
            host_port,
            db_name.clone(),
            db_user.clone(),
            db_password.clone(),
        );

        // Wait for the database to be ready
        let timeout = Duration::from_secs(dialect.startup_timeout_secs());

        info!("Waiting for database {} to be ready...", id);

        let ready = self
            .wait_for_db_ready(
                &container_id,
                dialect.as_ref(),
                &db_name,
                &db_user,
                &db_password,
                timeout,
            )
            .await;

        if ready {
            // Run post-startup command if the dialect has one (e.g., create database for SQL Server)
            if let Some((cmd, args)) =
                dialect.post_startup_command(&db_name, &db_user, &db_password)
            {
                let env = dialect.cli_env_vars(&db_name, &db_user, &db_password);
                info!("Running post-startup setup for database {}", id);
                match self.docker.exec(&container_id, &cmd, &args, &env).await {
                    Ok(output) => {
                        if output.exit_code != Some(0) {
                            warn!(
                                "Post-startup command failed with exit code {:?}: {}",
                                output.exit_code, output.stderr
                            );
                            let _ = self.docker.destroy_container(&container_id).await;
                            return Err(AppError::Internal(
                                "Post-startup database setup failed".to_string(),
                            ));
                        }
                        debug!("Post-startup setup completed");
                    }
                    Err(e) => {
                        warn!("Post-startup command failed: {}", e);
                        let _ = self.docker.destroy_container(&container_id).await;
                        return Err(AppError::Internal(format!(
                            "Post-startup database setup failed: {}",
                            e
                        )));
                    }
                }
            }

            instance.status = InstanceStatus::Running;
            info!("Database {} is ready", id);
        } else {
            warn!("Database {} failed to become ready, cleaning up", id);
            let _ = self.docker.destroy_container(&container_id).await;
            return Err(AppError::Internal(
                "Database failed to start within timeout".to_string(),
            ));
        }

        // Store in metadata (persistent)
        let now = chrono::Utc::now();
        let stored = StoredInstance {
            db_id: id,
            dialect: dialect_name.to_string(),
            db_name: db_name.clone(),
            db_user: db_user.clone(),
            db_password: db_password.clone(),
            status: InstanceState::Active,
            container_id: Some(container_id.clone()),
            host_port: Some(host_port),
            created_at: now,
            last_activity: now,
            archived_at: None,
            backup_key: None,
            backup_size_bytes: None,
        };
        self.metadata.insert_instance(&stored)?;

        // Store in cache (fast access)
        {
            let mut instances = self.instances.write().await;
            instances.insert(id, instance.clone());
        }

        Ok(instance)
    }

    async fn wait_for_db_ready(
        &self,
        container_id: &str,
        dialect: &dyn Dialect,
        db_name: &str,
        user: &str,
        password: &str,
        timeout: Duration,
    ) -> bool {
        use std::time::Instant;

        let start = Instant::now();
        let check_interval = Duration::from_millis(1000);

        let (cmd, args) = dialect.health_check_command(db_name, user, password);
        let env = dialect.cli_env_vars(db_name, user, password);

        while start.elapsed() < timeout {
            // First check if container is still running
            match self.docker.is_running(container_id).await {
                Ok(true) => {}
                Ok(false) => {
                    warn!("Container {} is not running", container_id);
                    return false;
                }
                Err(e) => {
                    debug!("Error checking container status: {}", e);
                }
            }

            // Try the health check command
            match self.docker.exec(container_id, &cmd, &args, &env).await {
                Ok(output) => {
                    if output.exit_code == Some(0) {
                        debug!("Health check passed");
                        return true;
                    }
                    debug!(
                        "Health check failed with exit code {:?}: {}",
                        output.exit_code, output.stderr
                    );
                }
                Err(e) => {
                    debug!("Health check exec failed: {}", e);
                }
            }

            tokio::time::sleep(check_interval).await;
        }

        false
    }

    pub async fn get_instance(&self, id: Uuid) -> Result<DbInstance> {
        // First check cache
        {
            let instances = self.instances.read().await;
            if let Some(instance) = instances.get(&id) {
                return Ok(instance.clone());
            }
        }

        // Check metadata - might be archived
        if let Some(stored) = self.metadata.get_instance(id)? {
            match stored.status {
                InstanceState::Active => {
                    // Should be in cache but isn't - try to recover
                    return self.recover_single_instance(&stored).await;
                }
                InstanceState::Archived => {
                    // Return a pseudo-instance indicating it's archived
                    // The API layer should handle this case
                    return Err(AppError::DbNotFound);
                }
                InstanceState::Restoring => {
                    return Err(AppError::RestoreInProgress);
                }
            }
        }

        Err(AppError::DbNotFound)
    }

    /// Get stored instance metadata (includes archived instances)
    pub fn get_stored_instance(&self, id: Uuid) -> Result<Option<StoredInstance>> {
        self.metadata.get_instance(id)
    }

    pub async fn touch_instance(&self, id: Uuid) -> Result<()> {
        // Update cache
        {
            let mut instances = self.instances.write().await;
            if let Some(instance) = instances.get_mut(&id) {
                instance.touch();
            }
        }
        // Update metadata
        self.metadata.touch_activity(id)?;
        Ok(())
    }

    /// Archive an instance: dump database, upload to R2, destroy container
    pub async fn archive_instance(&self, id: Uuid) -> Result<()> {
        let backup = match &self.backup {
            Some(b) => b,
            None => {
                // No backup configured - just destroy
                return self.destroy_instance(id).await;
            }
        };

        let stored = self
            .metadata
            .get_instance(id)?
            .ok_or(AppError::DbNotFound)?;

        let dialect = get_dialect(&stored.dialect)?;

        // Check if dialect supports backup
        if !dialect.supports_backup() {
            info!(
                "Dialect {} does not support backup, destroying instance {}",
                stored.dialect, id
            );
            return self.destroy_instance(id).await;
        }

        let container_id = stored
            .container_id
            .as_ref()
            .ok_or_else(|| AppError::Internal("No container ID for instance".to_string()))?;

        info!("Archiving instance {} (dialect: {})", id, stored.dialect);

        // 1. Dump database
        let (cmd, args) = dialect.dump_command(&stored.db_name, &stored.db_user, &stored.db_password);
        let env = dialect.cli_env_vars(&stored.db_name, &stored.db_user, &stored.db_password);

        let output = self.docker.exec(container_id, &cmd, &args, &env).await?;

        if output.exit_code != Some(0) {
            warn!(
                "Database dump failed for {}: {}",
                id, output.stderr
            );
            // Still destroy the container even if dump fails
            let _ = self.destroy_instance(id).await;
            return Err(AppError::BackupFailed(format!(
                "Dump failed: {}",
                output.stderr
            )));
        }

        // 2. Upload to R2 (compression is handled by BackupManager)
        let (key, size) = backup
            .upload_backup(id, output.stdout.as_bytes())
            .await?;

        info!(
            "Uploaded backup for {} to {} ({} bytes)",
            id, key, size
        );

        // 3. Update metadata
        self.metadata.mark_archived(id, &key, size)?;

        // 4. Remove from cache
        {
            let mut instances = self.instances.write().await;
            instances.remove(&id);
        }

        // 5. Destroy container
        self.docker.destroy_container(container_id).await?;

        info!("Instance {} archived successfully", id);

        Ok(())
    }

    /// Restore an instance from backup
    async fn restore_instance(&self, stored: &StoredInstance) -> Result<DbInstance> {
        let backup = self
            .backup
            .as_ref()
            .ok_or_else(|| AppError::RestoreFailed("Backup not configured".to_string()))?;

        let backup_key = stored
            .backup_key
            .as_ref()
            .ok_or(AppError::BackupNotFound)?;

        info!("Restoring instance {} from {}", stored.db_id, backup_key);

        // 1. Mark as restoring
        self.metadata
            .update_status(stored.db_id, InstanceState::Restoring)?;

        let dialect = get_dialect(&stored.dialect)?;

        // 2. Create new container
        let env_vars = dialect.env_vars(&stored.db_name, &stored.db_user, &stored.db_password);

        let mut labels = HashMap::new();
        labels.insert("db-api.id".to_string(), stored.db_id.to_string());
        labels.insert("db-api.dialect".to_string(), stored.dialect.clone());
        labels.insert("db-api.db_name".to_string(), stored.db_name.clone());
        labels.insert("db-api.db_user".to_string(), stored.db_user.clone());
        labels.insert("db-api.db_password".to_string(), stored.db_password.clone());
        labels.insert(
            "db-api.container_port".to_string(),
            dialect.default_port().to_string(),
        );

        let (container_id, host_port) = self
            .docker
            .create_container(
                stored.db_id,
                dialect.docker_image(),
                env_vars,
                dialect.default_port(),
                self.config.container_memory_mb,
                labels,
            )
            .await
            .map_err(|e| {
                // Revert status on failure
                let _ = self
                    .metadata
                    .update_status(stored.db_id, InstanceState::Archived);
                e
            })?;

        // 3. Wait for database ready
        let timeout = Duration::from_secs(dialect.startup_timeout_secs());
        let ready = self
            .wait_for_db_ready(
                &container_id,
                dialect.as_ref(),
                &stored.db_name,
                &stored.db_user,
                &stored.db_password,
                timeout,
            )
            .await;

        if !ready {
            let _ = self.docker.destroy_container(&container_id).await;
            let _ = self
                .metadata
                .update_status(stored.db_id, InstanceState::Archived);
            return Err(AppError::RestoreFailed(
                "Database failed to start".to_string(),
            ));
        }

        // 4. Run post-startup if needed
        if let Some((cmd, args)) =
            dialect.post_startup_command(&stored.db_name, &stored.db_user, &stored.db_password)
        {
            let env = dialect.cli_env_vars(&stored.db_name, &stored.db_user, &stored.db_password);
            let output = self.docker.exec(&container_id, &cmd, &args, &env).await?;
            if output.exit_code != Some(0) {
                let _ = self.docker.destroy_container(&container_id).await;
                let _ = self
                    .metadata
                    .update_status(stored.db_id, InstanceState::Archived);
                return Err(AppError::RestoreFailed(format!(
                    "Post-startup failed: {}",
                    output.stderr
                )));
            }
        }

        // 5. Download and restore backup
        let sql_data = backup.download_backup(backup_key).await?;

        let (cmd, args) =
            dialect.restore_command(&stored.db_name, &stored.db_user, &stored.db_password);
        let env = dialect.cli_env_vars(&stored.db_name, &stored.db_user, &stored.db_password);

        let output = self
            .docker
            .exec_with_stdin(&container_id, &cmd, &args, &env, &sql_data)
            .await
            .map_err(|e| {
                let _ = self.docker.destroy_container(&container_id);
                let _ = self
                    .metadata
                    .update_status(stored.db_id, InstanceState::Archived);
                AppError::RestoreFailed(format!("Restore exec failed: {}", e))
            })?;

        if output.exit_code != Some(0) {
            warn!(
                "Database restore failed for {}: {}",
                stored.db_id, output.stderr
            );
            let _ = self.docker.destroy_container(&container_id).await;
            let _ = self
                .metadata
                .update_status(stored.db_id, InstanceState::Archived);
            return Err(AppError::RestoreFailed(format!(
                "Restore failed: {}",
                output.stderr
            )));
        }

        // 6. Update metadata
        self.metadata
            .mark_active(stored.db_id, &container_id, host_port)?;

        // 7. Create instance and add to cache
        let mut instance = DbInstance::new(
            stored.db_id,
            stored.dialect.clone(),
            container_id,
            host_port,
            stored.db_name.clone(),
            stored.db_user.clone(),
            stored.db_password.clone(),
        );
        instance.status = InstanceStatus::Running;

        {
            let mut instances = self.instances.write().await;
            instances.insert(stored.db_id, instance.clone());
        }

        info!("Instance {} restored successfully", stored.db_id);

        Ok(instance)
    }

    /// Recover a single instance from metadata
    async fn recover_single_instance(&self, stored: &StoredInstance) -> Result<DbInstance> {
        let instance = DbInstance::new(
            stored.db_id,
            stored.dialect.clone(),
            stored.container_id.clone().unwrap_or_default(),
            stored.host_port.unwrap_or(0),
            stored.db_name.clone(),
            stored.db_user.clone(),
            stored.db_password.clone(),
        );

        let mut instances = self.instances.write().await;
        instances.insert(stored.db_id, instance.clone());

        Ok(instance)
    }

    pub async fn destroy_instance(&self, id: Uuid) -> Result<()> {
        // Remove from cache
        let instance = {
            let mut instances = self.instances.write().await;
            instances.remove(&id)
        };

        // Get container ID from cache or metadata
        let container_id = if let Some(inst) = instance {
            inst.container_id
        } else if let Some(stored) = self.metadata.get_instance(id)? {
            stored.container_id.unwrap_or_default()
        } else {
            return Err(AppError::DbNotFound);
        };

        if !container_id.is_empty() {
            info!("Destroying instance {}", id);
            let _ = self.docker.destroy_container(&container_id).await;
        }

        // Remove from metadata
        self.metadata.delete_instance(id)?;

        Ok(())
    }

    pub fn start_cleanup_task(self: Arc<Self>) {
        let manager = self.clone();
        let timeout = self.config.inactivity_timeout;

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(60));

            loop {
                ticker.tick().await;
                manager.cleanup_inactive(timeout).await;
            }
        });
    }

    async fn cleanup_inactive(&self, timeout: Duration) {
        // Get expired instances from metadata
        let expired = match self.metadata.get_expired_instances(timeout) {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to get expired instances: {}", e);
                return;
            }
        };

        for stored in expired {
            info!(
                "Instance {} has been idle, archiving",
                stored.db_id
            );

            // Archive instead of destroy (will fallback to destroy if backup not configured)
            if let Err(e) = self.archive_instance(stored.db_id).await {
                warn!("Failed to archive instance {}: {}", stored.db_id, e);
                // Try to destroy anyway
                let _ = self.destroy_instance(stored.db_id).await;
            }
        }
    }

    pub async fn instance_count(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Recover existing database containers on startup
    /// Now reconciles Docker state with SQLite metadata
    pub async fn recover_existing_instances(&self) -> Result<usize> {
        // First, load all active instances from metadata
        let stored_instances = self.metadata.list_active_instances()?;
        let mut recovered = 0;

        for stored in stored_instances {
            // Check if container still exists and is running
            if let Some(container_id) = &stored.container_id {
                match self.docker.is_running(container_id).await {
                    Ok(true) => {
                        // Container is running - add to cache
                        let instance = DbInstance::new(
                            stored.db_id,
                            stored.dialect.clone(),
                            container_id.clone(),
                            stored.host_port.unwrap_or(0),
                            stored.db_name.clone(),
                            stored.db_user.clone(),
                            stored.db_password.clone(),
                        );

                        info!(
                            "Recovered instance {} ({}) on port {:?}",
                            stored.db_id, stored.dialect, stored.host_port
                        );

                        let mut instances = self.instances.write().await;
                        instances.insert(stored.db_id, instance);
                        recovered += 1;
                    }
                    _ => {
                        // Container not running - mark as needing attention
                        warn!(
                            "Instance {} container not running, marking as archived",
                            stored.db_id
                        );
                        // If no backup, delete; otherwise mark archived
                        if stored.backup_key.is_some() {
                            let _ = self
                                .metadata
                                .update_status(stored.db_id, InstanceState::Archived);
                        } else {
                            let _ = self.metadata.delete_instance(stored.db_id);
                        }
                    }
                }
            }
        }

        // Also check for Docker containers not in metadata (legacy recovery)
        let containers = self.docker.list_db_containers().await?;
        for container in containers {
            if self.metadata.get_instance(container.db_id)?.is_none() {
                // Container exists but not in metadata - add it
                if container.is_running && container.host_port > 0 {
                    let now = chrono::Utc::now();
                    let stored = StoredInstance {
                        db_id: container.db_id,
                        dialect: container.dialect.clone(),
                        db_name: container.db_name.clone(),
                        db_user: container.db_user.clone(),
                        db_password: container.db_password.clone(),
                        status: InstanceState::Active,
                        container_id: Some(container.container_id.clone()),
                        host_port: Some(container.host_port),
                        created_at: now,
                        last_activity: now,
                        archived_at: None,
                        backup_key: None,
                        backup_size_bytes: None,
                    };

                    if self.metadata.insert_instance(&stored).is_ok() {
                        let instance = DbInstance::new(
                            container.db_id,
                            container.dialect,
                            container.container_id,
                            container.host_port,
                            container.db_name,
                            container.db_user,
                            container.db_password,
                        );

                        info!(
                            "Recovered legacy container {} on port {}",
                            stored.db_id, container.host_port
                        );

                        let mut instances = self.instances.write().await;
                        instances.insert(stored.db_id, instance);
                        recovered += 1;
                    }
                }
            }
        }

        Ok(recovered)
    }
}

/// Generate a strong password for database access
fn generate_password() -> String {
    // SQL Server requires strong passwords: uppercase, lowercase, numbers, and special chars
    // Use a UUID-based approach that satisfies these requirements
    let uuid = Uuid::new_v4().to_string();
    format!("Pwd{}!@#", uuid.replace("-", ""))
}
