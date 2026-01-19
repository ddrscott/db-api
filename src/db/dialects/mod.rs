mod mysql;
mod sqlserver;

use crate::error::{AppError, Result};

pub use mysql::MySqlDialect;
pub use sqlserver::SqlServerDialect;

/// Trait defining database dialect behavior
pub trait Dialect: Send + Sync {
    /// Dialect name (e.g., "mysql", "postgres", "sqlserver")
    fn name(&self) -> &'static str;

    /// Docker image to use
    fn docker_image(&self) -> &'static str;

    /// Default port inside the container
    fn default_port(&self) -> u16;

    /// Environment variables for container initialization
    fn env_vars(&self, db_name: &str, user: &str, password: &str) -> Vec<(String, String)>;

    /// Build the CLI command to execute a query inside the container
    /// Returns (executable, args) where args includes the query
    fn cli_command(&self, db_name: &str, user: &str, password: &str, query: &str) -> (String, Vec<String>);

    /// Parse CLI output into structured format
    /// Returns true if this line indicates an error
    fn is_error_line(&self, line: &str) -> bool;

    /// Time to wait for database to be ready (some are slower)
    fn startup_timeout_secs(&self) -> u64 {
        60
    }

    /// Command to check if database is ready
    fn health_check_command(&self, db_name: &str, user: &str, password: &str) -> (String, Vec<String>);

    /// Environment variables for CLI commands (e.g., password via env var to avoid warnings)
    fn cli_env_vars(&self, _db_name: &str, _user: &str, _password: &str) -> Vec<(String, String)> {
        vec![]
    }

    /// Build the CLI command for pretty text output (ASCII tables)
    /// Default implementation falls back to cli_command
    fn cli_command_text(&self, db_name: &str, user: &str, password: &str, query: &str) -> (String, Vec<String>) {
        self.cli_command(db_name, user, password, query)
    }
}

/// Get a dialect implementation by name
pub fn get_dialect(name: &str) -> Result<Box<dyn Dialect>> {
    match name.to_lowercase().as_str() {
        "mysql" | "mariadb" => Ok(Box::new(MySqlDialect)),
        "sqlserver" | "mssql" => Ok(Box::new(SqlServerDialect)),
        _ => Err(AppError::DialectUnsupported(name.to_string())),
    }
}

/// List of supported dialect names
pub fn supported_dialects() -> Vec<&'static str> {
    vec!["mysql", "sqlserver"]
}
