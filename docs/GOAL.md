# Goal

A RESTful API with SSE support for SQL query execution across any database dialect.

## Supported Databases

Currently implemented:
- **MySQL / MariaDB** - Full support via `mysql` CLI
- **Microsoft SQL Server** - x86-64 only via `sqlcmd`

Planned (any database with a Docker image and CLI can be added):
- [ ] PostgreSQL - via `psql` CLI
- [ ] SQLite - in-memory or file-based via `sqlite3` CLI
- [ ] Oracle Database - x86-64 only via `sqlplus` CLI
- [ ] CockroachDB - PostgreSQL-compatible via `cockroach sql` CLI
- [ ] TimescaleDB - PostgreSQL extension via `psql` CLI
- [ ] ClickHouse - via `clickhouse-client` CLI
- [ ] DuckDB - via `duckdb` CLI

### Architecture Notes
- Uses `docker exec` with database CLI tools (no native drivers needed)
- Adding a new database requires implementing the `Dialect` trait
- Some databases only have x86-64 images and won't run on ARM64 platforms:
  - **SQL Server**: x86-64 only. Azure SQL Edge supports ARM64 but lacks CLI tools.
  - **Oracle**: x86-64 only for most images.

## Core Concepts

### Ephemeral Database Instances
- Databases are short-lived and created/destroyed on demand
- Instances stay warm between requests for fast iteration (unlike db-fiddle which reinitializes every request)
- Automatic cleanup after 30 minutes of inactivity (configurable)
- Snapshot taken before cleanup for later restoration

### Security Model
- **Security via UUID obscurity** (similar to GitHub Gists)
- No authentication required - if you have the URL, you have access
- Each database instance is fully isolated with unique credentials
- No list/enumerate endpoint by design

### Use Cases
- Development and testing SQL queries across dialects
- Snippet sharing (like a pastebin for SQL)
- Interview/quiz platforms for SQL knowledge testing
- Web-based SQL editors
- NOT for production workloads

## Resource Limits

| Limit | Default | Configurable |
|-------|---------|--------------|
| Inactivity timeout | 30 min | Yes |
| Query timeout | 60 sec | Yes |
| Max database size | 10 MB | Yes (Docker) |
| Max concurrent connections | 10 | Yes |


## Tech Stack

- **API Server**: Rust (compiled, lightweight, self-hosted via docker-compose)
- **Session/State**: Cloudflare (KV/D1 for session metadata when deployed)
- **Database Engines**: Docker containers per dialect
- **Local Cache**: Redis (optional, for instance state)
- **Backup Storage**: Cloudflare R2 (no critical data on API server)
- **Monitoring**: Prometheus metrics endpoint

## Internal Architecture

### Database Initialization
Each new database instance is bootstrapped with dialect-specific init scripts that:
- Create a unique database name (derived from db_id)
- Create an isolated user with credentials scoped to that instance
- Apply any dialect-specific configuration (timeouts, memory limits)

Users receive a blank database - no user-facing init script support.

## API Structure

### Database Lifecycle

#### POST /db/new
Creates a new database instance.

**Request:**
```json
{
  "dialect": "mysql"
}
```

**Response:**
```json
{
  "db_id": "550e8400-e29b-41d4-a716-446655440000",
  "dialect": "mysql",
  "status": "ready"
}
```

#### GET /db/{db_id}
Returns the status of a database instance.

**Response:**
```json
{
  "db_id": "550e8400-e29b-41d4-a716-446655440000",
  "dialect": "mysql",
  "status": "running",
  "created_at": "2024-01-15T10:30:00Z",
  "last_activity": "2024-01-15T10:45:00Z",
  "expires_at": "2024-01-15T11:15:00Z"
}
```

#### DELETE /db/{db_id}
Manually destroys a database instance.

**Response:**
```json
{
  "db_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "destroyed"
}
```

### Query Execution

#### POST /db/{db_id}/query
Executes SQL query(ies) and streams results via SSE.

**Request:**
```json
{
  "query": "SELECT * FROM users; SELECT COUNT(*) FROM orders;"
}
```

**Response:** `Content-Type: text/event-stream`

SSE Event Types:
- `line` - Text output (CLI-style messages, notices, errors)
- `record` - Structured row data

```
event: line
data: {"text": "CREATE TABLE"}

event: record
data: {"columns": ["id", "name", "email"], "row": [1, "Alice", "alice@example.com"]}

event: record
data: {"columns": ["id", "name", "email"], "row": [2, "Bob", "bob@example.com"]}

event: line
data: {"text": "2 rows returned"}
```

Statement results vary by type:
- **DDL** (CREATE, ALTER, DROP): `line` with success/failure
- **DML SELECT**: `record` events per row
- **DML INSERT/UPDATE/DELETE**: `line` with affected row count
- **Transactions**: `line` with status

### Backup & Restore

Backups are stored in Cloudflare R2 and expire after 1 year.

#### POST /db/{db_id}/backup
Creates a snapshot of the database.

**Response:**
```json
{
  "backup_id": "660e8400-e29b-41d4-a716-446655440001",
  "db_id": "550e8400-e29b-41d4-a716-446655440000",
  "created_at": "2024-01-15T10:50:00Z",
  "expires_at": "2025-01-15T10:50:00Z",
  "size_bytes": 2048
}
```

#### GET /db/{db_id}/backup/{backup_id}
Downloads the backup as a file.

#### POST /db/{db_id}/restore/{backup_id}
Restores database from a backup (overwrites current state).

**Response:**
```json
{
  "db_id": "550e8400-e29b-41d4-a716-446655440000",
  "backup_id": "660e8400-e29b-41d4-a716-446655440001",
  "status": "restored"
}
```

#### POST /db/{db_id}/fork
Creates a new database instance from the current state of an existing one.

**Response:**
```json
{
  "db_id": "770e8400-e29b-41d4-a716-446655440002",
  "forked_from": "550e8400-e29b-41d4-a716-446655440000",
  "dialect": "mysql",
  "status": "ready"
}
```

### Operations

#### GET /health
Health check for monitoring.

**Response:**
```json
{
  "status": "healthy",
  "docker": "connected",
  "redis": "connected"
}
```

#### GET /metrics
Prometheus metrics endpoint.

## Error Handling

Fail-fast with SQL-CLI-style error messages.

**Error Response Format:**
```json
{
  "error": {
    "code": "QUERY_TIMEOUT",
    "message": "Query exceeded 60 second timeout",
    "detail": "Statement was cancelled after 60000ms"
  }
}
```

**Error Codes:**
| Code | HTTP Status | Description |
|------|-------------|-------------|
| `DB_NOT_FOUND` | 404 | Database instance does not exist |
| `DIALECT_UNSUPPORTED` | 400 | Requested dialect not available |
| `DIALECT_PULL_FAILED` | 503 | Docker image pull failed |
| `QUERY_TIMEOUT` | 408 | Query exceeded timeout limit |
| `QUERY_SYNTAX_ERROR` | 400 | SQL syntax error (includes DB error message) |
| `DB_SIZE_EXCEEDED` | 413 | Database exceeded size limit |
| `BACKUP_NOT_FOUND` | 404 | Backup does not exist |
| `BACKUP_EXPIRED` | 410 | Backup has expired (1 year retention) |
| `INTERNAL_ERROR` | 500 | Unexpected server error |

