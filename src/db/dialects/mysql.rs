use super::Dialect;

pub struct MySqlDialect;

impl Dialect for MySqlDialect {
    fn name(&self) -> &'static str {
        "mysql"
    }

    fn docker_image(&self) -> &'static str {
        "mysql:8"
    }

    fn default_port(&self) -> u16 {
        3306
    }

    fn env_vars(&self, db_name: &str, user: &str, password: &str) -> Vec<(String, String)> {
        vec![
            ("MYSQL_ROOT_PASSWORD".to_string(), password.to_string()),
            ("MYSQL_DATABASE".to_string(), db_name.to_string()),
            ("MYSQL_USER".to_string(), user.to_string()),
            ("MYSQL_PASSWORD".to_string(), password.to_string()),
        ]
    }

    fn cli_command(&self, db_name: &str, user: &str, _password: &str, query: &str) -> (String, Vec<String>) {
        // Password is passed via MYSQL_PWD env var to avoid CLI warning
        (
            "mysql".to_string(),
            vec![
                "-u".to_string(),
                user.to_string(),
                db_name.to_string(),
                "-e".to_string(),
                query.to_string(),
                // Output in tab-separated format for easier parsing
                "--batch".to_string(),
                "--raw".to_string(),
            ],
        )
    }

    fn cli_env_vars(&self, _db_name: &str, _user: &str, password: &str) -> Vec<(String, String)> {
        vec![("MYSQL_PWD".to_string(), password.to_string())]
    }

    fn is_error_line(&self, line: &str) -> bool {
        line.starts_with("ERROR") || line.contains("error:")
    }

    fn health_check_command(&self, db_name: &str, user: &str, _password: &str) -> (String, Vec<String>) {
        // Password is passed via MYSQL_PWD env var
        (
            "mysql".to_string(),
            vec![
                "-u".to_string(),
                user.to_string(),
                db_name.to_string(),
                "-e".to_string(),
                "SELECT 1".to_string(),
            ],
        )
    }

    fn cli_command_text(&self, db_name: &str, user: &str, _password: &str, query: &str) -> (String, Vec<String>) {
        // Password is passed via MYSQL_PWD env var
        // Use --table for pretty ASCII output with borders
        (
            "mysql".to_string(),
            vec![
                "-u".to_string(),
                user.to_string(),
                db_name.to_string(),
                "-e".to_string(),
                query.to_string(),
                "--table".to_string(),
            ],
        )
    }

    fn supports_backup(&self) -> bool {
        true
    }

    fn dump_command(&self, db_name: &str, user: &str, _password: &str) -> (String, Vec<String>) {
        // Password is passed via MYSQL_PWD env var
        // Use --single-transaction for consistent dump without locking
        (
            "mysqldump".to_string(),
            vec![
                "-u".to_string(),
                user.to_string(),
                "--single-transaction".to_string(),
                "--routines".to_string(),
                "--triggers".to_string(),
                db_name.to_string(),
            ],
        )
    }

    fn restore_command(&self, db_name: &str, user: &str, _password: &str) -> (String, Vec<String>) {
        // Password is passed via MYSQL_PWD env var
        // Reads SQL from stdin
        (
            "mysql".to_string(),
            vec!["-u".to_string(), user.to_string(), db_name.to_string()],
        )
    }

    // Pool container methods

    fn create_database_sql(&self, db_name: &str) -> String {
        format!("CREATE DATABASE `{}`", db_name)
    }

    fn drop_database_sql(&self, db_name: &str) -> String {
        format!("DROP DATABASE IF EXISTS `{}`", db_name)
    }

    fn create_user_sql(&self, user: &str, password: &str, db_name: &str) -> String {
        format!(
            "CREATE USER '{}'@'%' IDENTIFIED BY '{}'; GRANT ALL PRIVILEGES ON `{}`.* TO '{}'@'%'; FLUSH PRIVILEGES;",
            user, password, db_name, user
        )
    }

    fn drop_user_sql(&self, user: &str) -> String {
        format!("DROP USER IF EXISTS '{}'@'%'", user)
    }

    fn root_user(&self) -> &str {
        "root"
    }

    fn root_password_env(&self) -> &str {
        "MYSQL_ROOT_PASSWORD"
    }

    fn pool_env_vars(&self, root_password: &str) -> Vec<(String, String)> {
        vec![
            ("MYSQL_ROOT_PASSWORD".to_string(), root_password.to_string()),
        ]
    }

    fn exec_sql_command(&self, root_password: &str, sql: &str) -> (String, Vec<String>) {
        (
            "mysql".to_string(),
            vec![
                "-u".to_string(),
                "root".to_string(),
                format!("-p{}", root_password),
                "-e".to_string(),
                sql.to_string(),
            ],
        )
    }
}
