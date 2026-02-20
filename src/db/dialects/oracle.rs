use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use super::Dialect;

/// Oracle Database Free dialect
///
/// Uses the official Oracle container: container-registry.oracle.com/database/free:latest-lite
/// Fixed values for Free edition:
/// - ORACLE_SID = FREE
/// - ORACLE_PDB = FREEPDB1
///
/// NOTE: Oracle Database Free requires significant resources:
/// - Minimum 4GB RAM recommended
/// - Minimum 1GB shared memory (--shm-size)
/// - Startup time ~60-120 seconds
/// - ARM64 support available starting with Oracle 23.5 Free
pub struct OracleDialect;

// Full path to sqlplus in Oracle Free 26ai container
const SQLPLUS_PATH: &str = "/opt/oracle/product/26ai/dbhomeFree/bin/sqlplus";

impl Dialect for OracleDialect {
    fn name(&self) -> &'static str {
        "oracle"
    }

    fn docker_image(&self) -> &'static str {
        "container-registry.oracle.com/database/free:latest-lite"
    }

    fn default_port(&self) -> u16 {
        1521
    }

    fn env_vars(&self, _db_name: &str, _user: &str, password: &str) -> Vec<(String, String)> {
        // Oracle Free has fixed SID=FREE and PDB=FREEPDB1
        // Only password can be configured
        vec![("ORACLE_PWD".to_string(), password.to_string())]
    }

    fn cli_command(
        &self,
        _db_name: &str,
        user: &str,
        password: &str,
        query: &str,
    ) -> (String, Vec<String>) {
        // Use sqlplus with connection string to FREEPDB1
        // PAGESIZE must be > 0 to include headers (0 disables headers)
        // Use base64 encoding to avoid shell escaping issues with single quotes
        // Ensure query ends with semicolon (Oracle requires it to separate from EXIT)
        let query_trimmed = query.trim();
        let query_with_semi = if query_trimmed.ends_with(';') {
            query_trimmed.to_string()
        } else {
            format!("{};", query_trimmed)
        };
        // LINESIZE prevents line wrapping which breaks tab-separated parsing
        let sql_script = format!(
            "SET PAGESIZE 50000\nSET LINESIZE 32767\nSET FEEDBACK OFF\nSET HEADING ON\nSET COLSEP '\t'\n{}\nEXIT;",
            query_with_semi
        );
        let encoded = BASE64.encode(sql_script.as_bytes());
        (
            "bash".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "echo '{}' | base64 -d | {} -S {}@//localhost:1521/FREEPDB1",
                    encoded,
                    SQLPLUS_PATH,
                    self.format_login(user, password)
                ),
            ],
        )
    }

    fn cli_env_vars(&self, _db_name: &str, _user: &str, _password: &str) -> Vec<(String, String)> {
        // Oracle sqlplus doesn't use env vars for password in the same way
        // Password is passed in connection string
        vec![]
    }

    fn is_error_line(&self, line: &str) -> bool {
        line.starts_with("ORA-")
            || line.starts_with("SP2-")
            || line.starts_with("ERROR")
            || line.contains("error:")
    }

    fn startup_timeout_secs(&self) -> u64 {
        180 // Oracle takes longer to start (2-3 minutes)
    }

    fn min_memory_mb(&self) -> u32 {
        4096 // Oracle requires minimum 4GB RAM
    }

    fn shm_size_mb(&self) -> u32 {
        1024 // Oracle needs 1GB shared memory
    }

    fn health_check_command(
        &self,
        _db_name: &str,
        _user: &str,
        _password: &str,
    ) -> (String, Vec<String>) {
        // Health check using root credentials against PDB
        // Password passed via ORACLE_PWD env var during exec
        (
            "bash".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "echo 'SELECT 1 FROM DUAL;' | {} -S sys/$ORACLE_PWD@//localhost:1521/FREEPDB1 as sysdba",
                    SQLPLUS_PATH
                ),
            ],
        )
    }

    fn cli_command_text(
        &self,
        _db_name: &str,
        user: &str,
        password: &str,
        query: &str,
    ) -> (String, Vec<String>) {
        // Pretty formatted output with column headers
        // Use base64 encoding to avoid shell escaping issues
        // Ensure query ends with semicolon (Oracle requires it to separate from EXIT)
        let query_trimmed = query.trim();
        let query_with_semi = if query_trimmed.ends_with(';') {
            query_trimmed.to_string()
        } else {
            format!("{};", query_trimmed)
        };
        let sql_script = format!(
            "SET LINESIZE 200\nSET PAGESIZE 50\nCOLUMN dummy FORMAT A50\n{}\nEXIT;",
            query_with_semi
        );
        let encoded = BASE64.encode(sql_script.as_bytes());
        (
            "bash".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "echo '{}' | base64 -d | {} -S {}@//localhost:1521/FREEPDB1",
                    encoded,
                    SQLPLUS_PATH,
                    self.format_login(user, password)
                ),
            ],
        )
    }

    fn supports_backup(&self) -> bool {
        true
    }

    fn dump_command(&self, _db_name: &str, user: &str, password: &str) -> (String, Vec<String>) {
        // Use expdp (Data Pump) for export - but it writes to files, not stdout
        // For simple backup, we'll export schema DDL via dbms_metadata
        // This is a simplified approach; full Data Pump would require more setup
        // Use base64 encoding to avoid shell escaping issues
        let sql_script = format!(
            "SET LONG 1000000\nSET PAGESIZE 0\nSET FEEDBACK OFF\n\
             SELECT DBMS_METADATA.GET_DDL(object_type, object_name, '{}') \
             FROM all_objects WHERE owner = UPPER('{}') \
             AND object_type IN ('TABLE', 'INDEX', 'SEQUENCE', 'VIEW', 'PROCEDURE', 'FUNCTION');\n\
             EXIT;",
            user.to_uppercase(),
            user.to_uppercase()
        );
        let encoded = BASE64.encode(sql_script.as_bytes());
        (
            "bash".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "echo '{}' | base64 -d | {} -S {}@//localhost:1521/FREEPDB1",
                    encoded,
                    SQLPLUS_PATH,
                    self.format_login(user, password)
                ),
            ],
        )
    }

    fn restore_command(&self, _db_name: &str, user: &str, password: &str) -> (String, Vec<String>) {
        // Pipe SQL statements to sqlplus
        (
            "bash".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "{} -S {}@//localhost:1521/FREEPDB1",
                    SQLPLUS_PATH,
                    self.format_login(user, password)
                ),
            ],
        )
    }

    // Pool container methods

    fn create_database_sql(&self, db_name: &str) -> String {
        // Oracle Free uses pluggable database FREEPDB1
        // We create a user/schema instead of a database
        // The "database" name becomes the schema/user name
        // Note: Oracle Free doesn't have USERS tablespace, use default
        format!(
            "CREATE USER {} IDENTIFIED BY \"temp_pwd_123\" QUOTA UNLIMITED ON SYSTEM",
            db_name.to_uppercase()
        )
    }

    fn drop_database_sql(&self, db_name: &str) -> String {
        // Drop the schema/user and all its objects
        format!("DROP USER {} CASCADE", db_name.to_uppercase())
    }

    fn create_user_sql(&self, user: &str, password: &str, db_name: &str) -> String {
        // In Oracle, we need to:
        // 1. Drop the temp user created by create_database_sql
        // 2. Create the actual user with the right password
        // 3. Grant necessary privileges
        //
        // Note: db_name was used as the schema name in create_database_sql
        // but the actual user needs different credentials
        // Oracle Free doesn't have USERS tablespace
        format!(
            "BEGIN \
               EXECUTE IMMEDIATE 'DROP USER {} CASCADE'; \
             EXCEPTION WHEN OTHERS THEN NULL; \
             END;\n/\n\
             CREATE USER {} IDENTIFIED BY \"{}\" QUOTA UNLIMITED ON SYSTEM;\n\
             GRANT CONNECT, RESOURCE, CREATE SESSION, CREATE TABLE, CREATE VIEW, \
                   CREATE SEQUENCE, CREATE PROCEDURE, CREATE TRIGGER TO {};",
            db_name.to_uppercase(),
            user.to_uppercase(),
            password,
            user.to_uppercase()
        )
    }

    fn drop_user_sql(&self, user: &str) -> String {
        format!("DROP USER {} CASCADE", user.to_uppercase())
    }

    fn root_user(&self) -> &str {
        "sys"
    }

    fn root_password_env(&self) -> &str {
        "ORACLE_PWD"
    }

    fn pool_env_vars(&self, root_password: &str) -> Vec<(String, String)> {
        vec![("ORACLE_PWD".to_string(), root_password.to_string())]
    }

    fn exec_sql_command(&self, root_password: &str, sql: &str) -> (String, Vec<String>) {
        // Execute SQL as sysdba against the PDB
        // Use bash with pipefail to ensure sqlplus errors propagate
        // Add WHENEVER SQLERROR EXIT SQL.SQLCODE to fail on SQL errors
        // Use base64 encoding to avoid shell escaping issues
        let sql_script = format!("WHENEVER SQLERROR EXIT SQL.SQLCODE\n{}", sql);
        let encoded = BASE64.encode(sql_script.as_bytes());
        (
            "bash".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "set -o pipefail; echo '{}' | base64 -d | {} -S sys/{}@//localhost:1521/FREEPDB1 as sysdba",
                    encoded,
                    SQLPLUS_PATH,
                    root_password
                ),
            ],
        )
    }
}

impl OracleDialect {
    /// Format login string for sqlplus (user/password)
    fn format_login(&self, user: &str, password: &str) -> String {
        format!("{}/{}", user, password)
    }
}
