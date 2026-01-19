use futures::stream;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::Stream;
use tracing::debug;

use crate::docker::DockerManager;
use crate::error::Result;

use super::dialects::get_dialect;
use super::instance::DbInstance;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum QueryEvent {
    Line { text: String },
    Record { columns: Vec<String>, row: Vec<JsonValue> },
    Error { message: String },
    Done { affected_rows: Option<u64> },
}

pub struct QueryExecutor {
    docker: Arc<DockerManager>,
    query_timeout: Duration,
}

/// Raw query output (stdout + stderr)
#[derive(Debug)]
pub struct RawQueryOutput {
    pub stdout: String,
    pub stderr: String,
}

impl QueryExecutor {
    pub fn new(docker: Arc<DockerManager>, query_timeout: Duration) -> Self {
        Self {
            docker,
            query_timeout,
        }
    }

    /// Execute query and return raw CLI output (for format=text)
    /// Uses pretty ASCII table format with borders
    pub async fn execute_raw(
        &self,
        instance: &DbInstance,
        sql: &str,
    ) -> Result<RawQueryOutput> {
        let dialect = get_dialect(&instance.dialect)?;

        // Use text-formatted command for pretty output
        let (cmd, args) = dialect.cli_command_text(
            &instance.db_name,
            &instance.db_user,
            &instance.db_password,
            sql,
        );

        let env = dialect.cli_env_vars(
            &instance.db_name,
            &instance.db_user,
            &instance.db_password,
        );

        debug!("Executing query via CLI (text): {} {:?}", cmd, args);

        let output = self
            .docker
            .exec_with_timeout(&instance.container_id, &cmd, &args, &env, self.query_timeout)
            .await?;

        Ok(RawQueryOutput {
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    /// Execute query and return parsed events (for format=json, jsonl)
    pub async fn execute(
        &self,
        instance: &DbInstance,
        sql: &str,
    ) -> Result<impl Stream<Item = QueryEvent>> {
        let dialect = get_dialect(&instance.dialect)?;

        let (cmd, args) = dialect.cli_command(
            &instance.db_name,
            &instance.db_user,
            &instance.db_password,
            sql,
        );

        let env = dialect.cli_env_vars(
            &instance.db_name,
            &instance.db_user,
            &instance.db_password,
        );

        debug!("Executing query via CLI: {} {:?}", cmd, args);

        let output = self
            .docker
            .exec_with_timeout(&instance.container_id, &cmd, &args, &env, self.query_timeout)
            .await?;

        let events = parse_cli_output(&output.stdout, &output.stderr, dialect.as_ref());

        Ok(stream::iter(events))
    }
}

/// Parse CLI output into QueryEvents
fn parse_cli_output(
    stdout: &str,
    stderr: &str,
    dialect: &dyn super::dialects::Dialect,
) -> Vec<QueryEvent> {
    let mut events = Vec::new();

    // Handle stderr (errors/warnings)
    for line in stderr.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if dialect.is_error_line(line) {
            events.push(QueryEvent::Error {
                message: line.to_string(),
            });
        } else {
            // Warnings or notices
            events.push(QueryEvent::Line {
                text: line.to_string(),
            });
        }
    }

    // Handle stdout (results)
    let lines: Vec<&str> = stdout.lines().collect();

    if lines.is_empty() {
        events.push(QueryEvent::Done { affected_rows: None });
        return events;
    }

    // Check if first line looks like headers (tab-separated column names)
    let mut line_iter = lines.iter().peekable();

    while let Some(line) = line_iter.next() {
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        // Check for result messages
        if line.starts_with("Query OK")
            || line.starts_with("Rows matched")
            || line.contains("row(s) affected")
            || line.contains("rows affected")
        {
            events.push(QueryEvent::Line {
                text: line.to_string(),
            });
            continue;
        }

        // Check for error lines in stdout
        if dialect.is_error_line(line) {
            events.push(QueryEvent::Error {
                message: line.to_string(),
            });
            continue;
        }

        // Try to parse as tab-separated data
        if line.contains('\t') {
            // This could be a header row or a data row
            let columns: Vec<String> = line.split('\t').map(|s| s.to_string()).collect();

            // Peek at next line to see if this is a header
            if let Some(next_line) = line_iter.peek() {
                let next_line = next_line.trim();
                if next_line.contains('\t') || next_line.is_empty() {
                    // This is likely a header, emit subsequent rows as records
                    let header = columns.clone();

                    // Skip separator lines (e.g., "---\t---\t---")
                    while let Some(data_line) = line_iter.next() {
                        let data_line = data_line.trim();
                        if data_line.is_empty() {
                            break;
                        }
                        if data_line.chars().all(|c| c == '-' || c == '\t' || c == '+' || c == ' ') {
                            continue;
                        }

                        let values: Vec<JsonValue> = data_line
                            .split('\t')
                            .map(|s| parse_value(s.trim()))
                            .collect();

                        events.push(QueryEvent::Record {
                            columns: header.clone(),
                            row: values,
                        });
                    }
                    continue;
                }
            }

            // Single row without header context
            events.push(QueryEvent::Line {
                text: line.to_string(),
            });
        } else {
            // Plain text line
            events.push(QueryEvent::Line {
                text: line.to_string(),
            });
        }
    }

    events.push(QueryEvent::Done { affected_rows: None });
    events
}

/// Parse a string value into a JSON value
fn parse_value(s: &str) -> JsonValue {
    if s.eq_ignore_ascii_case("null") || s.is_empty() {
        return JsonValue::Null;
    }

    // Try integer
    if let Ok(n) = s.parse::<i64>() {
        return JsonValue::Number(n.into());
    }

    // Try float
    if let Ok(n) = s.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return JsonValue::Number(num);
        }
    }

    // Try boolean
    if s.eq_ignore_ascii_case("true") || s == "1" {
        return JsonValue::Bool(true);
    }
    if s.eq_ignore_ascii_case("false") || s == "0" {
        return JsonValue::Bool(false);
    }

    // Default to string
    JsonValue::String(s.to_string())
}
