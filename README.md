# db-api

A RESTful API for executing SQL queries across multiple database dialects using ephemeral Docker containers. Perfect for development, testing, SQL playgrounds, and interview platforms.

## Quick Start

```bash
# Prerequisites: Docker running

# Run the API
make run

# Create a MySQL database
curl -X POST http://localhost:8081/db/new \
  -H "Content-Type: application/json" \
  -d '{"dialect": "mysql"}'

# Execute a query (replace {db_id} with the returned id)
curl -X POST http://localhost:8081/db/{db_id}/query \
  -H "Content-Type: application/json" \
  -d '{"sql": "CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100))"}'

curl -X POST http://localhost:8081/db/{db_id}/query \
  -H "Content-Type: application/json" \
  -d '{"sql": "INSERT INTO users VALUES (1, '\''Alice'\''), (2, '\''Bob'\'')"}'

curl -X POST http://localhost:8081/db/{db_id}/query \
  -H "Content-Type: application/json" \
  -d '{"sql": "SELECT * FROM users"}'
```

## Features

- **Multiple Dialects**: MySQL 8, SQL Server (more coming)
- **Ephemeral Instances**: Auto-cleanup after 30 minutes of inactivity
- **Streaming Responses**: Server-Sent Events for real-time query output
- **Container Pooling**: 80x faster database creation
- **Backup/Restore**: Snapshot databases to Cloudflare R2
- **Fork Databases**: Clone existing database state
- **Zero Auth by Design**: URL-based access control (like GitHub Gists)

## API Reference

### Database Lifecycle

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/db/new` | POST | Create a new database instance |
| `/db/{db_id}` | GET | Get instance status and metadata |
| `/db/{db_id}` | DELETE | Destroy instance immediately |
| `/db/{db_id}/fork` | POST | Clone database to new instance |

### Query Execution

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/db/{db_id}/query` | POST | Execute SQL with streaming SSE response |

**Query Parameters:**
- `sql` (required): SQL statement to execute
- `format`: Output format - `text` (default), `json`, `jsonl`

### Backup & Restore

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/db/{db_id}/backup` | POST | Create backup snapshot |
| `/db/{db_id}/backup/{backup_id}` | GET | Download backup file |
| `/db/{db_id}/restore/{backup_id}` | POST | Restore from backup |

### Operations

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Docker connectivity check |
| `/metrics` | GET | Prometheus metrics |
| `/openapi.json` | GET | OpenAPI 3.0 spec |
| `/docs` | GET | Swagger UI |

## Configuration

Environment variables (see `.env.example`):

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `8080` | HTTP port |
| `INACTIVITY_TIMEOUT_SECS` | `1800` | Auto-cleanup timeout (30 min) |
| `QUERY_TIMEOUT_SECS` | `60` | Query execution timeout |
| `CONTAINER_MEMORY_MB` | `512` | Memory limit per container |
| `MAX_DB_SIZE_MB` | `10` | Database size limit |
| `METADATA_DB_PATH` | `/data/metadata.db` | SQLite metadata location |
| `RUST_LOG` | `info` | Log level |

**Optional Backup (Cloudflare R2):**
```
R2_ACCOUNT_ID=<your_account>
R2_ACCESS_KEY_ID=<your_key>
R2_SECRET_ACCESS_KEY=<your_secret>
R2_BUCKET=db-api-backups
BACKUP_ON_EXPIRY=true
```

## Development

```bash
# Build and run
make run              # Debug mode on port 8081
make release          # Optimized build

# Code quality
make fmt              # Format code
make lint             # Run clippy
make test             # Run tests

# Integration tests
make test-health      # Health check
make test-mysql       # Full MySQL workflow

# Docker
make docker-build     # Build image
make docker-run       # Run container
docker-compose up -d  # Production deployment
```

## Architecture

```
src/
├── api/              # HTTP endpoints (Axum)
├── db/               # Database logic
│   ├── manager.rs    # Instance lifecycle
│   ├── query.rs      # Query execution
│   └── dialects/     # MySQL, SQL Server implementations
├── docker/           # Container management (Bollard)
├── storage/          # Metadata (SQLite) & backups (R2)
└── config.rs         # Environment configuration
```

**Key Design Decisions:**
- **Container Pooling**: One container per dialect hosts multiple databases for fast creation
- **CLI Execution**: Uses `docker exec` with native CLI tools (mysql, sqlcmd) instead of drivers
- **SSE Streaming**: Real-time output for long-running queries
- **UUID Security**: Database IDs are unguessable UUIDs - no auth needed

## Supported Dialects

| Dialect | Status | Notes |
|---------|--------|-------|
| MySQL 8 | Stable | Full support |
| SQL Server | Stable | x86-64 only |
| PostgreSQL | Planned | |
| SQLite | Planned | |

## Use Cases

- SQL playgrounds and web editors
- Interview/quiz platforms
- Development and testing
- Teaching SQL concepts

**Not For:**
- Production workloads
- Persistent data storage
- High-security applications

## License

MIT
