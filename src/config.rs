use std::env;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    pub inactivity_timeout: Duration,
    pub query_timeout: Duration,
    pub container_memory_mb: u32,
    pub max_db_size_mb: u32,
    pub max_connections: u32,

    // Storage configuration
    pub metadata_db_path: String,

    // R2/S3 backup configuration
    pub r2_account_id: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
    pub r2_bucket: String,

    // Feature flags
    pub backup_on_expiry: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host: env::var("HOST")
                .ok()
                .and_then(|s| IpAddr::from_str(&s).ok())
                .unwrap_or_else(|| IpAddr::from_str("0.0.0.0").unwrap()),
            port: env::var("PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8080),
            inactivity_timeout: Duration::from_secs(
                env::var("INACTIVITY_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1800),
            ),
            query_timeout: Duration::from_secs(
                env::var("QUERY_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60),
            ),
            container_memory_mb: env::var("CONTAINER_MEMORY_MB")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(512),
            max_db_size_mb: env::var("MAX_DB_SIZE_MB")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10),
            max_connections: env::var("MAX_CONNECTIONS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10),

            // Storage
            metadata_db_path: env::var("METADATA_DB_PATH")
                .unwrap_or_else(|_| "/data/metadata.db".to_string()),

            // R2
            r2_account_id: env::var("R2_ACCOUNT_ID").unwrap_or_default(),
            r2_access_key_id: env::var("R2_ACCESS_KEY_ID").unwrap_or_default(),
            r2_secret_access_key: env::var("R2_SECRET_ACCESS_KEY").unwrap_or_default(),
            r2_bucket: env::var("R2_BUCKET").unwrap_or_else(|_| "db-api-backups".to_string()),

            // Features
            backup_on_expiry: env::var("BACKUP_ON_EXPIRY")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(true),
        }
    }

    /// Check if backup is enabled and configured
    pub fn backup_enabled(&self) -> bool {
        self.backup_on_expiry
            && !self.r2_account_id.is_empty()
            && !self.r2_access_key_id.is_empty()
            && !self.r2_secret_access_key.is_empty()
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}
