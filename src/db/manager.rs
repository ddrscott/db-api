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

use super::dialects::{get_dialect, Dialect};
use super::instance::{DbInstance, InstanceStatus};

pub struct InstanceManager {
    instances: Arc<RwLock<HashMap<Uuid, DbInstance>>>,
    docker: Arc<DockerManager>,
    config: Config,
}

impl InstanceManager {
    pub fn new(docker: DockerManager, config: Config) -> Self {
        Self {
            instances: Arc::new(RwLock::new(HashMap::new())),
            docker: Arc::new(docker),
            config,
        }
    }

    pub fn docker(&self) -> Arc<DockerManager> {
        self.docker.clone()
    }

    pub async fn create_instance(&self, dialect_name: &str) -> Result<DbInstance> {
        let dialect = get_dialect(dialect_name)?;
        let id = Uuid::new_v4();

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
            if let Some((cmd, args)) = dialect.post_startup_command(&db_name, &db_user, &db_password) {
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
                        return Err(AppError::Internal(
                            format!("Post-startup database setup failed: {}", e),
                        ));
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

        // Store the instance
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
        let instances = self.instances.read().await;
        instances.get(&id).cloned().ok_or(AppError::DbNotFound)
    }

    pub async fn touch_instance(&self, id: Uuid) -> Result<()> {
        let mut instances = self.instances.write().await;
        if let Some(instance) = instances.get_mut(&id) {
            instance.touch();
            Ok(())
        } else {
            Err(AppError::DbNotFound)
        }
    }

    pub async fn destroy_instance(&self, id: Uuid) -> Result<()> {
        let instance = {
            let mut instances = self.instances.write().await;
            instances.remove(&id)
        };

        if let Some(instance) = instance {
            info!("Destroying instance {}", id);
            self.docker.destroy_container(&instance.container_id).await?;
            Ok(())
        } else {
            Err(AppError::DbNotFound)
        }
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
        let now = chrono::Utc::now();
        let mut to_remove = Vec::new();

        {
            let instances = self.instances.read().await;
            for (id, instance) in instances.iter() {
                let idle_duration = now
                    .signed_duration_since(instance.last_activity)
                    .to_std()
                    .unwrap_or(Duration::ZERO);

                if idle_duration > timeout && instance.status == InstanceStatus::Running {
                    info!(
                        "Instance {} has been idle for {:?}, marking for cleanup",
                        id, idle_duration
                    );
                    to_remove.push(*id);
                }
            }
        }

        for id in to_remove {
            if let Err(e) = self.destroy_instance(id).await {
                warn!("Failed to cleanup instance {}: {}", id, e);
            }
        }
    }

    pub async fn instance_count(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Recover existing database containers on startup
    pub async fn recover_existing_instances(&self) -> Result<usize> {
        let containers = self.docker.list_db_containers().await?;
        let mut recovered = 0;

        for container in containers {
            if !container.is_running {
                info!(
                    "Skipping stopped container {} (db_id: {})",
                    container.container_id, container.db_id
                );
                continue;
            }

            if container.host_port == 0 {
                warn!(
                    "Container {} has no port mapping, skipping",
                    container.container_id
                );
                continue;
            }

            let instance = DbInstance::new(
                container.db_id,
                container.dialect,
                container.container_id.clone(),
                container.host_port,
                container.db_name,
                container.db_user,
                container.db_password,
            );

            info!(
                "Recovered instance {} ({}) on port {}",
                container.db_id, instance.dialect, container.host_port
            );

            let mut instances = self.instances.write().await;
            instances.insert(container.db_id, instance);
            recovered += 1;
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
