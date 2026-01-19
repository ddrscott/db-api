use super::Dialect;

/// SQL Server dialect
///
/// NOTE: SQL Server only runs on x86-64 (amd64) platforms.
/// It does NOT work on ARM64 (Apple Silicon, AWS Graviton, etc.)
/// Azure SQL Edge supports ARM64 but doesn't include sqlcmd tools.
pub struct SqlServerDialect;

impl Dialect for SqlServerDialect {
    fn name(&self) -> &'static str {
        "sqlserver"
    }

    fn docker_image(&self) -> &'static str {
        "mcr.microsoft.com/mssql/server:2022-latest"
    }

    fn default_port(&self) -> u16 {
        1433
    }

    fn env_vars(&self, _db_name: &str, _user: &str, password: &str) -> Vec<(String, String)> {
        // SQL Server requires SA password and EULA acceptance
        // Database and user are created after startup via sqlcmd
        vec![
            ("ACCEPT_EULA".to_string(), "Y".to_string()),
            ("MSSQL_SA_PASSWORD".to_string(), password.to_string()),
        ]
    }

    fn cli_command(&self, db_name: &str, user: &str, _password: &str, query: &str) -> (String, Vec<String>) {
        // Password is passed via SQLCMDPASSWORD env var
        // Use the actual instance user, not 'sa' (which we don't have the password for in pool mode)
        (
            "/opt/mssql-tools18/bin/sqlcmd".to_string(),
            vec![
                "-S".to_string(),
                "localhost".to_string(),
                "-U".to_string(),
                user.to_string(),
                "-d".to_string(),
                db_name.to_string(),
                "-Q".to_string(),
                query.to_string(),
                // Tab-separated output, no headers count
                "-s".to_string(),
                "\t".to_string(),
                "-W".to_string(), // Remove trailing spaces
                "-C".to_string(), // Trust server certificate
            ],
        )
    }

    fn cli_env_vars(&self, _db_name: &str, _user: &str, password: &str) -> Vec<(String, String)> {
        vec![("SQLCMDPASSWORD".to_string(), password.to_string())]
    }

    fn is_error_line(&self, line: &str) -> bool {
        line.starts_with("Msg ") || line.contains("Error:") || line.starts_with("Sqlcmd: Error:")
    }

    fn startup_timeout_secs(&self) -> u64 {
        90 // SQL Server takes longer to start
    }

    fn health_check_command(&self, _db_name: &str, _user: &str, _password: &str) -> (String, Vec<String>) {
        // Password is passed via SQLCMDPASSWORD env var
        (
            "/opt/mssql-tools18/bin/sqlcmd".to_string(),
            vec![
                "-S".to_string(),
                "localhost".to_string(),
                "-U".to_string(),
                "sa".to_string(),
                "-Q".to_string(),
                "SELECT 1".to_string(),
                "-C".to_string(),
            ],
        )
    }

    fn post_startup_command(&self, db_name: &str, _user: &str, _password: &str) -> Option<(String, Vec<String>)> {
        // Create the database after SQL Server is ready
        // SQL Server doesn't auto-create databases via env vars like MySQL
        let create_db_sql = format!(
            "IF NOT EXISTS (SELECT name FROM sys.databases WHERE name = '{}') CREATE DATABASE [{}]",
            db_name, db_name
        );
        Some((
            "/opt/mssql-tools18/bin/sqlcmd".to_string(),
            vec![
                "-S".to_string(),
                "localhost".to_string(),
                "-U".to_string(),
                "sa".to_string(),
                "-Q".to_string(),
                create_db_sql,
                "-C".to_string(),
            ],
        ))
    }

    // Pool container methods

    fn create_database_sql(&self, db_name: &str) -> String {
        format!(
            "IF NOT EXISTS (SELECT name FROM sys.databases WHERE name = '{}') CREATE DATABASE [{}]",
            db_name, db_name
        )
    }

    fn drop_database_sql(&self, db_name: &str) -> String {
        format!(
            "IF EXISTS (SELECT name FROM sys.databases WHERE name = '{}') DROP DATABASE [{}]",
            db_name, db_name
        )
    }

    fn create_user_sql(&self, user: &str, password: &str, db_name: &str) -> String {
        // SQL Server requires: create login, then use the database, create user, grant permissions
        format!(
            "IF NOT EXISTS (SELECT name FROM sys.server_principals WHERE name = '{}') \
             CREATE LOGIN [{}] WITH PASSWORD = '{}'; \
             USE [{}]; \
             IF NOT EXISTS (SELECT name FROM sys.database_principals WHERE name = '{}') \
             CREATE USER [{}] FOR LOGIN [{}]; \
             ALTER ROLE db_owner ADD MEMBER [{}];",
            user, user, password, db_name, user, user, user, user
        )
    }

    fn drop_user_sql(&self, user: &str) -> String {
        format!(
            "IF EXISTS (SELECT name FROM sys.server_principals WHERE name = '{}') \
             DROP LOGIN [{}]",
            user, user
        )
    }

    fn root_user(&self) -> &str {
        "sa"
    }

    fn root_password_env(&self) -> &str {
        "MSSQL_SA_PASSWORD"
    }

    fn pool_env_vars(&self, root_password: &str) -> Vec<(String, String)> {
        vec![
            ("ACCEPT_EULA".to_string(), "Y".to_string()),
            ("MSSQL_SA_PASSWORD".to_string(), root_password.to_string()),
        ]
    }

    fn exec_sql_command(&self, root_password: &str, sql: &str) -> (String, Vec<String>) {
        (
            "/opt/mssql-tools18/bin/sqlcmd".to_string(),
            vec![
                "-S".to_string(),
                "localhost".to_string(),
                "-U".to_string(),
                "sa".to_string(),
                "-P".to_string(),
                root_password.to_string(),
                "-Q".to_string(),
                sql.to_string(),
                "-C".to_string(), // Trust server certificate
            ],
        )
    }
}
