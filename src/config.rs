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
        }
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}
