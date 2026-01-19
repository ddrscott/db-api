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
use crate::storage::{BackupManager, InstanceState, MetadataStore, PoolContainer, StoredInstance};

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

    pub fn metadata(&self) -> Arc<MetadataStore> {
        self.metadata.clone()
    }

    /// Get or create a pool container for the given dialect
    async fn get_or_create_pool_container(&self, dialect: &dyn Dialect) -> Result<PoolContainer> {
        let dialect_name = dialect.name();

        // Check if we have a pool container in metadata
        if let Some(pool) = self.metadata.get_pool_container(dialect_name)? {
            // Verify container is still running
            if self.docker.is_running(&pool.container_id).await.unwrap_or(false) {
                debug!("Using existing pool container for {}: {}", dialect_name, pool.container_id);
                return Ok(pool);
            }
            // Container died, remove stale record
            info!("Pool container {} for {} is not running, creating new one", pool.container_id, dialect_name);
            self.metadata.delete_pool_container(dialect_name)?;
        }

        // Create new pool container
        info!("Creating new pool container for {}", dialect_name);
        let root_password = generate_password();
        let env_vars = dialect.pool_env_vars(&root_password);

        let (container_id, host_port) = self
            .docker
            .create_pool_container(
                dialect_name,
                dialect.docker_image(),
                env_vars,
                dialect.default_port(),
                self.config.container_memory_mb,
            )
            .await?;

        // Wait for database to be ready (using a simple health check)
        let timeout = Duration::from_secs(dialect.startup_timeout_secs());
        info!("Waiting for pool container {} to be ready...", dialect_name);

        let ready = self
            .wait_for_pool_ready(&container_id, dialect, &root_password, timeout)
            .await;

        if !ready {
            warn!("Pool container {} failed to become ready, cleaning up", dialect_name);
            let _ = self.docker.destroy_container(&container_id).await;
            return Err(AppError::Internal(
                format!("Pool container for {} failed to start within timeout", dialect_name),
            ));
        }

        let pool = PoolContainer {
            dialect: dialect_name.to_string(),
            container_id: container_id.clone(),
            host_port,
            root_password,
            created_at: chrono::Utc::now(),
            status: "running".to_string(),
        };

        self.metadata.upsert_pool_container(&pool)?;
        info!("Pool container for {} ready on port {}", dialect_name, host_port);

        Ok(pool)
    }

    /// Wait for pool container to be ready
    async fn wait_for_pool_ready(
        &self,
        container_id: &str,
        dialect: &dyn Dialect,
        root_password: &str,
        timeout: Duration,
    ) -> bool {
        use std::time::Instant;

        let start = Instant::now();
        let check_interval = Duration::from_millis(1000);

        // For pool container, we check using root credentials
        let (cmd, args) = dialect.exec_sql_command(root_password, "SELECT 1");

        while start.elapsed() < timeout {
            // Check if container is still running
            match self.docker.is_running(container_id).await {
                Ok(true) => {}
                Ok(false) => {
                    warn!("Pool container {} is not running", container_id);
                    return false;
                }
                Err(e) => {
                    debug!("Error checking container status: {}", e);
                }
            }

            // Try the health check
            match self.docker.exec(container_id, &cmd, &args, &[]).await {
                Ok(output) => {
                    if output.exit_code == Some(0) {
                        debug!("Pool container health check passed");
                        return true;
                    }
                    debug!(
                        "Pool health check failed with exit code {:?}: {}",
                        output.exit_code, output.stderr
                    );
                }
                Err(e) => {
                    debug!("Pool health check exec failed: {}", e);
                }
            }

            tokio::time::sleep(check_interval).await;
        }

        false
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

        info!(
            "Creating {} instance {} with database {}",
            dialect_name, id, db_name
        );

        // Get or create pool container for this dialect
        let pool = self.get_or_create_pool_container(dialect.as_ref()).await?;

        // Create database inside the pool container
        let create_db_sql = dialect.create_database_sql(&db_name);
        let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &create_db_sql);

        debug!("Creating database {} in pool container", db_name);
        let output = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await?;

        if output.exit_code != Some(0) {
            warn!(
                "Failed to create database {}: {}",
                db_name, output.stderr
            );
            return Err(AppError::Internal(format!(
                "Failed to create database: {}",
                output.stderr
            )));
        }

        // Create user with permissions
        let create_user_sql = dialect.create_user_sql(&db_user, &db_password, &db_name);
        let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &create_user_sql);

        debug!("Creating user {} for database {}", db_user, db_name);
        let output = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await?;

        if output.exit_code != Some(0) {
            // Cleanup: drop the database we just created
            let drop_db_sql = dialect.drop_database_sql(&db_name);
            let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);
            let _ = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await;

            warn!(
                "Failed to create user {}: {}",
                db_user, output.stderr
            );
            return Err(AppError::Internal(format!(
                "Failed to create database user: {}",
                output.stderr
            )));
        }

        let mut instance = DbInstance::new(
            id,
            dialect_name.to_string(),
            pool.container_id.clone(),
            pool.host_port,
            db_name.clone(),
            db_user.clone(),
            db_password.clone(),
        );
        instance.status = InstanceStatus::Running;

        info!("Database {} created in pool container", id);

        // Store in metadata (persistent)
        let now = chrono::Utc::now();
        let stored = StoredInstance {
            db_id: id,
            dialect: dialect_name.to_string(),
            db_name: db_name.clone(),
            db_user: db_user.clone(),
            db_password: db_password.clone(),
            status: InstanceState::Active,
            container_id: Some(pool.container_id.clone()),
            host_port: Some(pool.host_port),
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

    /// Archive an instance: dump database, upload to R2, drop database from pool
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

        // Get pool container for this dialect
        let pool = self
            .metadata
            .get_pool_container(&stored.dialect)?
            .ok_or_else(|| AppError::Internal("No pool container for dialect".to_string()))?;

        info!("Archiving instance {} (dialect: {})", id, stored.dialect);

        // 1. Dump database (using user credentials)
        let (cmd, args) = dialect.dump_command(&stored.db_name, &stored.db_user, &stored.db_password);
        let env = dialect.cli_env_vars(&stored.db_name, &stored.db_user, &stored.db_password);

        let output = self.docker.exec(&pool.container_id, &cmd, &args, &env).await?;

        if output.exit_code != Some(0) {
            warn!(
                "Database dump failed for {}: {}",
                id, output.stderr
            );
            // Still drop the database even if dump fails
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

        // 5. Drop user and database from pool (not destroy container)
        let drop_user_sql = dialect.drop_user_sql(&stored.db_user);
        let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_user_sql);
        let _ = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await;

        let drop_db_sql = dialect.drop_database_sql(&stored.db_name);
        let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);
        let _ = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await;

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

        // 2. Get or create pool container (fast if already exists)
        let pool = self.get_or_create_pool_container(dialect.as_ref()).await
            .map_err(|e| {
                let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
                e
            })?;

        // 3. Create database in pool container
        let create_db_sql = dialect.create_database_sql(&stored.db_name);
        let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &create_db_sql);

        let output = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await
            .map_err(|e| {
                let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
                AppError::RestoreFailed(format!("Failed to create database: {}", e))
            })?;

        if output.exit_code != Some(0) {
            let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
            return Err(AppError::RestoreFailed(format!(
                "Failed to create database: {}",
                output.stderr
            )));
        }

        // 4. Create user with permissions
        let create_user_sql = dialect.create_user_sql(&stored.db_user, &stored.db_password, &stored.db_name);
        let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &create_user_sql);

        let output = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await
            .map_err(|e| {
                // Cleanup: drop the database
                let drop_db_sql = dialect.drop_database_sql(&stored.db_name);
                let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);
                let _ = futures::executor::block_on(self.docker.exec(&pool.container_id, &cmd, &args, &[]));
                let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
                AppError::RestoreFailed(format!("Failed to create user: {}", e))
            })?;

        if output.exit_code != Some(0) {
            // Cleanup: drop the database
            let drop_db_sql = dialect.drop_database_sql(&stored.db_name);
            let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);
            let _ = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await;
            let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
            return Err(AppError::RestoreFailed(format!(
                "Failed to create user: {}",
                output.stderr
            )));
        }

        // 5. Download and restore backup
        let sql_data = backup.download_backup(backup_key).await?;

        let (cmd, args) =
            dialect.restore_command(&stored.db_name, &stored.db_user, &stored.db_password);
        let env = dialect.cli_env_vars(&stored.db_name, &stored.db_user, &stored.db_password);

        let output = self
            .docker
            .exec_with_stdin(&pool.container_id, &cmd, &args, &env, &sql_data)
            .await
            .map_err(|e| {
                // Cleanup: drop user and database
                let drop_user_sql = dialect.drop_user_sql(&stored.db_user);
                let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_user_sql);
                let _ = futures::executor::block_on(self.docker.exec(&pool.container_id, &cmd, &args, &[]));
                let drop_db_sql = dialect.drop_database_sql(&stored.db_name);
                let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);
                let _ = futures::executor::block_on(self.docker.exec(&pool.container_id, &cmd, &args, &[]));
                let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
                AppError::RestoreFailed(format!("Restore exec failed: {}", e))
            })?;

        if output.exit_code != Some(0) {
            warn!(
                "Database restore failed for {}: {}",
                stored.db_id, output.stderr
            );
            // Cleanup: drop user and database
            let drop_user_sql = dialect.drop_user_sql(&stored.db_user);
            let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_user_sql);
            let _ = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await;
            let drop_db_sql = dialect.drop_database_sql(&stored.db_name);
            let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);
            let _ = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await;
            let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
            return Err(AppError::RestoreFailed(format!(
                "Restore failed: {}",
                output.stderr
            )));
        }

        // 6. Update metadata
        self.metadata
            .mark_active(stored.db_id, &pool.container_id, pool.host_port)?;

        // 7. Create instance and add to cache
        let mut instance = DbInstance::new(
            stored.db_id,
            stored.dialect.clone(),
            pool.container_id.clone(),
            pool.host_port,
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
        // Get instance info from cache or metadata
        let stored = self.metadata.get_instance(id)?.ok_or(AppError::DbNotFound)?;

        // Remove from cache
        {
            let mut instances = self.instances.write().await;
            instances.remove(&id);
        }

        // Get pool container for this dialect
        let dialect = get_dialect(&stored.dialect)?;
        if let Some(pool) = self.metadata.get_pool_container(&stored.dialect)? {
            // Drop user first
            let drop_user_sql = dialect.drop_user_sql(&stored.db_user);
            let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_user_sql);

            debug!("Dropping user {} for instance {}", stored.db_user, id);
            if let Err(e) = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await {
                warn!("Failed to drop user {}: {}", stored.db_user, e);
            }

            // Drop database
            let drop_db_sql = dialect.drop_database_sql(&stored.db_name);
            let (cmd, args) = dialect.exec_sql_command(&pool.root_password, &drop_db_sql);

            debug!("Dropping database {} for instance {}", stored.db_name, id);
            if let Err(e) = self.docker.exec(&pool.container_id, &cmd, &args, &[]).await {
                warn!("Failed to drop database {}: {}", stored.db_name, e);
            }

            info!("Instance {} destroyed (database dropped)", id);
        } else {
            // No pool container found - this might be a legacy instance or pool died
            warn!("No pool container found for dialect {}, can't drop database", stored.dialect);
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
    /// Now reconciles Docker state with SQLite metadata for pool containers
    pub async fn recover_existing_instances(&self) -> Result<usize> {
        // First, recover pool containers
        let stored_pools = self.metadata.list_pool_containers()?;
        for pool in stored_pools {
            // Check if pool container still exists and is running
            match self.docker.is_running(&pool.container_id).await {
                Ok(true) => {
                    info!(
                        "Pool container for {} recovered on port {}",
                        pool.dialect, pool.host_port
                    );
                }
                _ => {
                    // Pool container died - remove from metadata
                    // Instances using it will need to be recreated
                    warn!(
                        "Pool container for {} not running, removing from metadata",
                        pool.dialect
                    );
                    let _ = self.metadata.delete_pool_container(&pool.dialect);
                }
            }
        }

        // Also check for running pool containers not in metadata (e.g., API restarted but containers persisted)
        let running_pools = self.docker.list_pool_containers().await?;
        for pool in running_pools {
            if self.metadata.get_pool_container(&pool.dialect)?.is_none() {
                // Pool container exists but not in metadata - we can't use it
                // because we don't know the root password. Destroy it.
                warn!(
                    "Found orphaned pool container for {}, destroying",
                    pool.dialect
                );
                let _ = self.docker.destroy_container(&pool.container_id).await;
            }
        }

        // Load all active instances from metadata
        let stored_instances = self.metadata.list_active_instances()?;
        let mut recovered = 0;

        for stored in stored_instances {
            // Check if the pool container for this dialect is running
            if let Some(pool) = self.metadata.get_pool_container(&stored.dialect)? {
                if self.docker.is_running(&pool.container_id).await.unwrap_or(false) {
                    // Pool is running - add instance to cache
                    let instance = DbInstance::new(
                        stored.db_id,
                        stored.dialect.clone(),
                        pool.container_id.clone(),
                        pool.host_port,
                        stored.db_name.clone(),
                        stored.db_user.clone(),
                        stored.db_password.clone(),
                    );

                    info!(
                        "Recovered instance {} ({}) on port {}",
                        stored.db_id, stored.dialect, pool.host_port
                    );

                    let mut instances = self.instances.write().await;
                    instances.insert(stored.db_id, instance);
                    recovered += 1;
                } else {
                    // Pool not running - mark instance as orphaned
                    warn!(
                        "Instance {} pool container not running",
                        stored.db_id
                    );
                    if stored.backup_key.is_some() {
                        let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
                    } else {
                        let _ = self.metadata.delete_instance(stored.db_id);
                    }
                }
            } else {
                // No pool container for this dialect
                warn!(
                    "No pool container for instance {} ({})",
                    stored.db_id, stored.dialect
                );
                if stored.backup_key.is_some() {
                    let _ = self.metadata.update_status(stored.db_id, InstanceState::Archived);
                } else {
                    let _ = self.metadata.delete_instance(stored.db_id);
                }
            }
        }

        // Also check for legacy Docker containers not in metadata
        let containers = self.docker.list_db_containers().await?;
        for container in containers {
            // Legacy containers should be destroyed - we now use pool containers
            if container.is_running {
                warn!(
                    "Found legacy container for instance {}, destroying",
                    container.db_id
                );
                let _ = self.docker.destroy_container(&container.container_id).await;
            }
            // Clean up metadata if present
            if self.metadata.get_instance(container.db_id)?.is_some() {
                let _ = self.metadata.delete_instance(container.db_id);
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
