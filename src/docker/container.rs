use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, PortBinding};
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::{AppError, Result};

pub struct DockerManager {
    docker: Docker,
}

/// Output from a docker exec command
#[derive(Debug)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
}

/// Discovered container info for recovery
#[derive(Debug)]
pub struct DiscoveredContainer {
    pub container_id: String,
    pub db_id: Uuid,
    pub dialect: String,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    pub host_port: u16,
    pub is_running: bool,
}

/// Discovered pool container info
#[derive(Debug)]
pub struct DiscoveredPoolContainer {
    pub container_id: String,
    pub dialect: String,
    pub host_port: u16,
    pub is_running: bool,
}

impl DockerManager {
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(Self { docker })
    }

    pub async fn health_check(&self) -> Result<bool> {
        self.docker.ping().await?;
        Ok(true)
    }

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        info!("Pulling image: {}", image);

        let options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!("Pull status: {}", status);
                    }
                }
                Err(e) => {
                    return Err(AppError::DialectPullFailed(e.to_string()));
                }
            }
        }

        info!("Image pulled successfully: {}", image);
        Ok(())
    }

    pub async fn create_container(
        &self,
        db_id: Uuid,
        image: &str,
        env_vars: Vec<(String, String)>,
        container_port: u16,
        memory_limit_mb: u32,
        labels: HashMap<String, String>,
    ) -> Result<(String, u16)> {
        let container_name = format!("db-api-{}", db_id);

        // Check if image exists locally, pull if not
        if self.docker.inspect_image(image).await.is_err() {
            self.pull_image(image).await?;
        }

        let env: Vec<String> = env_vars
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        let port_key = format!("{}/tcp", container_port);
        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            port_key.clone(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some("0".to_string()), // Let Docker assign a port
            }]),
        );

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            memory: Some((memory_limit_mb as i64) * 1024 * 1024),
            ..Default::default()
        };

        let mut exposed_ports = HashMap::new();
        exposed_ports.insert(port_key.clone(), HashMap::new());

        let config = Config {
            image: Some(image.to_string()),
            env: Some(env),
            exposed_ports: Some(exposed_ports),
            host_config: Some(host_config),
            labels: Some(labels),
            ..Default::default()
        };

        let options = CreateContainerOptions {
            name: &container_name,
            platform: None,
        };

        let response = self.docker.create_container(Some(options), config).await?;
        let container_id = response.id;

        info!("Created container: {} ({})", container_name, container_id);

        // Start the container
        self.docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await?;

        info!("Started container: {}", container_id);

        // Get the assigned host port
        let inspect = self.docker.inspect_container(&container_id, None).await?;
        let host_port = inspect
            .network_settings
            .and_then(|ns| ns.ports)
            .and_then(|ports| ports.get(&format!("{}/tcp", container_port)).cloned())
            .flatten()
            .and_then(|bindings| bindings.first().cloned())
            .and_then(|binding| binding.host_port)
            .and_then(|port| port.parse::<u16>().ok())
            .ok_or_else(|| AppError::Internal("Failed to get container port".to_string()))?;

        info!("Container {} mapped to host port {}", container_id, host_port);

        Ok((container_id, host_port))
    }

    /// Execute a command inside a container and return the output
    pub async fn exec(
        &self,
        container_id: &str,
        cmd: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<ExecOutput> {
        let mut full_cmd = vec![cmd.to_string()];
        full_cmd.extend(args.iter().cloned());

        debug!("Executing in container {}: {:?}", container_id, full_cmd);

        let env_vars: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        let exec_options = CreateExecOptions {
            cmd: Some(full_cmd),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            env: if env_vars.is_empty() {
                None
            } else {
                Some(env_vars)
            },
            ..Default::default()
        };

        let exec = self.docker.create_exec(container_id, exec_options).await?;

        let start_result = self.docker.start_exec(&exec.id, None).await?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = start_result {
            while let Some(msg) = output.next().await {
                match msg {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Error reading exec output: {}", e);
                    }
                }
            }
        }

        // Get exit code
        let inspect = self.docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code;

        debug!(
            "Exec completed with exit code {:?}, stdout len: {}, stderr len: {}",
            exit_code,
            stdout.len(),
            stderr.len()
        );

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }

    /// Execute a command with timeout
    pub async fn exec_with_timeout(
        &self,
        container_id: &str,
        cmd: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ExecOutput> {
        match tokio::time::timeout(timeout, self.exec(container_id, cmd, args, env)).await {
            Ok(result) => result,
            Err(_) => Err(AppError::QueryTimeout),
        }
    }

    /// Execute a command with stdin data piped in
    /// Used for database restore operations where SQL is piped to the client
    pub async fn exec_with_stdin(
        &self,
        container_id: &str,
        cmd: &str,
        args: &[String],
        env: &[(String, String)],
        stdin_data: &[u8],
    ) -> Result<ExecOutput> {
        use bollard::exec::StartExecOptions;
        use tokio::io::AsyncWriteExt;

        let mut full_cmd = vec![cmd.to_string()];
        full_cmd.extend(args.iter().cloned());

        debug!(
            "Executing with stdin in container {}: {:?} ({} bytes)",
            container_id,
            full_cmd,
            stdin_data.len()
        );

        let env_vars: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        let exec_options = CreateExecOptions {
            cmd: Some(full_cmd),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            env: if env_vars.is_empty() {
                None
            } else {
                Some(env_vars)
            },
            ..Default::default()
        };

        let exec = self.docker.create_exec(container_id, exec_options).await?;

        let start_options = StartExecOptions {
            detach: false,
            ..Default::default()
        };

        let start_result = self
            .docker
            .start_exec(&exec.id, Some(start_options))
            .await?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached {
            mut output,
            mut input,
        } = start_result
        {
            // Write stdin data
            input.write_all(stdin_data).await.map_err(|e| {
                AppError::RestoreFailed(format!("Failed to write stdin: {}", e))
            })?;
            input.shutdown().await.map_err(|e| {
                AppError::RestoreFailed(format!("Failed to close stdin: {}", e))
            })?;

            // Read output
            while let Some(msg) = output.next().await {
                match msg {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Error reading exec output: {}", e);
                    }
                }
            }
        }

        // Get exit code
        let inspect = self.docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code;

        debug!(
            "Exec with stdin completed with exit code {:?}, stdout len: {}, stderr len: {}",
            exit_code,
            stdout.len(),
            stderr.len()
        );

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }

    pub async fn stop_container(&self, container_id: &str) -> Result<()> {
        info!("Stopping container: {}", container_id);

        let options = StopContainerOptions { t: 10 };

        match self.docker.stop_container(container_id, Some(options)).await {
            Ok(_) => {
                info!("Stopped container: {}", container_id);
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => {
                // Container already stopped
                warn!("Container {} was already stopped", container_id);
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn remove_container(&self, container_id: &str) -> Result<()> {
        info!("Removing container: {}", container_id);

        let options = RemoveContainerOptions {
            force: true,
            v: true, // Remove volumes
            ..Default::default()
        };

        self.docker
            .remove_container(container_id, Some(options))
            .await?;

        info!("Removed container: {}", container_id);
        Ok(())
    }

    pub async fn destroy_container(&self, container_id: &str) -> Result<()> {
        // Stop first, then remove
        let _ = self.stop_container(container_id).await;
        self.remove_container(container_id).await
    }

    /// Check if container is running
    pub async fn is_running(&self, container_id: &str) -> Result<bool> {
        let inspect = self.docker.inspect_container(container_id, None).await?;
        Ok(inspect.state.and_then(|s| s.running).unwrap_or(false))
    }

    /// Check if a container exists (running or not)
    pub async fn container_exists(&self, container_id: &str) -> bool {
        self.docker.inspect_container(container_id, None).await.is_ok()
    }

    /// Create a pool container for a dialect
    pub async fn create_pool_container(
        &self,
        dialect_name: &str,
        image: &str,
        env_vars: Vec<(String, String)>,
        container_port: u16,
        memory_limit_mb: u32,
    ) -> Result<(String, u16)> {
        let container_name = format!("db-api-pool-{}", dialect_name);

        // Check if image exists locally, pull if not
        if self.docker.inspect_image(image).await.is_err() {
            self.pull_image(image).await?;
        }

        let env: Vec<String> = env_vars
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        let port_key = format!("{}/tcp", container_port);
        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            port_key.clone(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some("0".to_string()), // Let Docker assign a port
            }]),
        );

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            memory: Some((memory_limit_mb as i64) * 1024 * 1024),
            ..Default::default()
        };

        let mut exposed_ports = HashMap::new();
        exposed_ports.insert(port_key.clone(), HashMap::new());

        // Labels for pool container identification
        let mut labels = HashMap::new();
        labels.insert("db-api.pool".to_string(), "true".to_string());
        labels.insert("db-api.dialect".to_string(), dialect_name.to_string());
        labels.insert("db-api.container_port".to_string(), container_port.to_string());

        let config = Config {
            image: Some(image.to_string()),
            env: Some(env),
            exposed_ports: Some(exposed_ports),
            host_config: Some(host_config),
            labels: Some(labels),
            ..Default::default()
        };

        let options = CreateContainerOptions {
            name: &container_name,
            platform: None,
        };

        let response = self.docker.create_container(Some(options), config).await?;
        let container_id = response.id;

        info!("Created pool container: {} ({})", container_name, container_id);

        // Start the container
        self.docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await?;

        info!("Started pool container: {}", container_id);

        // Get the assigned host port
        let inspect = self.docker.inspect_container(&container_id, None).await?;
        let host_port = inspect
            .network_settings
            .and_then(|ns| ns.ports)
            .and_then(|ports| ports.get(&format!("{}/tcp", container_port)).cloned())
            .flatten()
            .and_then(|bindings| bindings.first().cloned())
            .and_then(|binding| binding.host_port)
            .and_then(|port| port.parse::<u16>().ok())
            .ok_or_else(|| AppError::Internal("Failed to get pool container port".to_string()))?;

        info!("Pool container {} mapped to host port {}", container_id, host_port);

        Ok((container_id, host_port))
    }

    /// List all db-api pool containers
    pub async fn list_pool_containers(&self) -> Result<Vec<DiscoveredPoolContainer>> {
        use bollard::container::ListContainersOptions;

        let mut filters = HashMap::new();
        filters.insert("name", vec!["db-api-pool-"]);

        let options = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };

        let containers = self.docker.list_containers(Some(options)).await?;
        let mut result = Vec::new();

        for container in containers {
            let container_id = match &container.id {
                Some(id) => id.clone(),
                None => continue,
            };

            let inspect = match self.docker.inspect_container(&container_id, None).await {
                Ok(i) => i,
                Err(e) => {
                    warn!("Failed to inspect pool container {}: {}", container_id, e);
                    continue;
                }
            };

            let labels = inspect.config.as_ref().and_then(|c| c.labels.as_ref());

            // Check if it's a pool container
            let is_pool = labels
                .and_then(|l| l.get("db-api.pool"))
                .map(|v| v == "true")
                .unwrap_or(false);

            if !is_pool {
                continue;
            }

            let dialect = match labels.and_then(|l| l.get("db-api.dialect")) {
                Some(d) => d.clone(),
                None => continue,
            };

            let container_port_str = labels.and_then(|l| l.get("db-api.container_port"));
            let container_port: u16 = container_port_str
                .and_then(|s| s.parse().ok())
                .unwrap_or(3306);

            let host_port = inspect
                .network_settings
                .as_ref()
                .and_then(|ns| ns.ports.as_ref())
                .and_then(|ports| ports.get(&format!("{}/tcp", container_port)))
                .and_then(|bindings| bindings.as_ref())
                .and_then(|bindings| bindings.first())
                .and_then(|binding| binding.host_port.as_ref())
                .and_then(|port| port.parse::<u16>().ok())
                .unwrap_or(0);

            let is_running = inspect
                .state
                .as_ref()
                .and_then(|s| s.running)
                .unwrap_or(false);

            result.push(DiscoveredPoolContainer {
                container_id,
                dialect,
                host_port,
                is_running,
            });
        }

        Ok(result)
    }

    /// List all db-api containers and extract their metadata
    pub async fn list_db_containers(&self) -> Result<Vec<DiscoveredContainer>> {
        use bollard::container::ListContainersOptions;

        let mut filters = HashMap::new();
        filters.insert("name", vec!["db-api-"]);

        let options = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };

        let containers = self.docker.list_containers(Some(options)).await?;
        let mut result = Vec::new();

        for container in containers {
            let container_id = match &container.id {
                Some(id) => id.clone(),
                None => continue,
            };

            // Get full container details for labels and port mappings
            let inspect = match self.docker.inspect_container(&container_id, None).await {
                Ok(i) => i,
                Err(e) => {
                    warn!("Failed to inspect container {}: {}", container_id, e);
                    continue;
                }
            };

            let labels = inspect.config.as_ref().and_then(|c| c.labels.as_ref());

            // Extract our labels
            let db_id = labels
                .and_then(|l| l.get("db-api.id"))
                .and_then(|s| Uuid::parse_str(s).ok());
            let dialect = labels.and_then(|l| l.get("db-api.dialect")).cloned();
            let db_name = labels.and_then(|l| l.get("db-api.db_name")).cloned();
            let db_user = labels.and_then(|l| l.get("db-api.db_user")).cloned();
            let db_password = labels.and_then(|l| l.get("db-api.db_password")).cloned();

            // All labels must be present
            let (db_id, dialect, db_name, db_user, db_password) =
                match (db_id, dialect, db_name, db_user, db_password) {
                    (Some(id), Some(d), Some(n), Some(u), Some(p)) => (id, d, n, u, p),
                    _ => {
                        debug!("Container {} missing required labels, skipping", container_id);
                        continue;
                    }
                };

            // Get port from container info
            let container_port_str = labels.and_then(|l| l.get("db-api.container_port"));
            let container_port: u16 = container_port_str
                .and_then(|s| s.parse().ok())
                .unwrap_or(3306);

            let host_port = inspect
                .network_settings
                .as_ref()
                .and_then(|ns| ns.ports.as_ref())
                .and_then(|ports| ports.get(&format!("{}/tcp", container_port)))
                .and_then(|bindings| bindings.as_ref())
                .and_then(|bindings| bindings.first())
                .and_then(|binding| binding.host_port.as_ref())
                .and_then(|port| port.parse::<u16>().ok())
                .unwrap_or(0);

            let is_running = inspect
                .state
                .as_ref()
                .and_then(|s| s.running)
                .unwrap_or(false);

            result.push(DiscoveredContainer {
                container_id,
                db_id,
                dialect,
                db_name,
                db_user,
                db_password,
                host_port,
                is_running,
            });
        }

        Ok(result)
    }
}
