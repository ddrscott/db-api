use axum::{
    http::{header, StatusCode},
    response::{Response, sse::{Event, KeepAlive, Sse}},
    Json,
};
use futures::stream::Stream;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::convert::Infallible;

use crate::db::query::QueryEvent;
use crate::db::RawQueryOutput;

// ============================================================================
// SSE Response (format=jsonl, transport=sse)
// ============================================================================

pub fn query_event_to_sse(event: QueryEvent) -> Result<Event, Infallible> {
    let (event_type, data) = match &event {
        QueryEvent::Line { .. } => ("line", serde_json::to_string(&event).unwrap()),
        QueryEvent::Record { .. } => ("record", serde_json::to_string(&event).unwrap()),
        QueryEvent::Error { .. } => ("error", serde_json::to_string(&event).unwrap()),
        QueryEvent::Done { .. } => ("done", serde_json::to_string(&event).unwrap()),
    };

    Ok(Event::default().event(event_type).data(data))
}

pub fn create_sse_response<S>(stream: S) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = QueryEvent> + Send + 'static,
{
    use futures::StreamExt;

    let sse_stream = stream.map(query_event_to_sse);

    Sse::new(sse_stream).keep_alive(KeepAlive::default())
}

// ============================================================================
// Text Response (format=text)
// ============================================================================

pub fn create_text_response(output: RawQueryOutput) -> Response {
    // Combine stderr and stdout, with stderr first if present
    let mut body = if output.stderr.is_empty() {
        output.stdout
    } else if output.stdout.is_empty() {
        output.stderr
    } else {
        format!("{}\n{}", output.stderr.trim_end(), output.stdout)
    };

    // Add '---' separators between multiple result sets
    // MySQL tables end with +---+ and the next table starts with +---+
    // We detect this pattern and add a separator
    body = add_result_separators(&body);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body.into())
        .unwrap()
}

/// Add '---' separators between multiple result sets in text output
fn add_result_separators(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 2 {
        return text.to_string();
    }

    let mut result = Vec::new();
    let mut prev_was_table_end = false;

    for line in lines {
        let is_table_border = line.starts_with('+') && line.ends_with('+') && line.contains('-');

        // If previous line was a table border (end) and this is also a border (start of new table)
        if prev_was_table_end && is_table_border {
            result.push("---");
        }

        result.push(line);
        prev_was_table_end = is_table_border;
    }

    result.join("\n")
}

// ============================================================================
// JSON Response (format=json)
// ============================================================================

#[derive(Debug, Serialize)]
pub struct JsonQueryResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Vec<JsonValue>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
}

impl Default for JsonQueryResponse {
    fn default() -> Self {
        Self {
            columns: None,
            rows: None,
            affected_rows: None,
            error: None,
            messages: Vec::new(),
        }
    }
}

pub fn create_json_response(events: Vec<QueryEvent>) -> Json<JsonQueryResponse> {
    let mut response = JsonQueryResponse::default();
    let mut columns: Option<Vec<String>> = None;
    let mut rows: Vec<Vec<JsonValue>> = Vec::new();

    for event in events {
        match event {
            QueryEvent::Line { text } => {
                response.messages.push(text);
            }
            QueryEvent::Record { columns: cols, row } => {
                if columns.is_none() {
                    columns = Some(cols);
                }
                rows.push(row);
            }
            QueryEvent::Error { message } => {
                // Collect all errors into one message
                if let Some(existing) = &response.error {
                    response.error = Some(format!("{}\n{}", existing, message));
                } else {
                    response.error = Some(message);
                }
            }
            QueryEvent::Done { affected_rows } => {
                response.affected_rows = affected_rows;
            }
        }
    }

    if !rows.is_empty() {
        response.columns = columns;
        response.rows = Some(rows);
    }

    Json(response)
}
