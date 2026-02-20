# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
make run              # Run server (debug mode, port 8081)
make release          # Build optimized binary
make fmt              # Format code
make lint             # Run clippy (cargo clippy -- -W warnings)
make test             # Run unit tests
cargo test <name>     # Run single test by name

# Integration tests (requires running server)
make test-health      # Health endpoint check
make test-mysql       # Full MySQL workflow

# Docker
make docker-build     # Build image
make docker-run       # Build and run container
docker-compose up -d  # Production deployment
```

## Architecture

**Rust API server** using Axum that manages ephemeral database instances via Docker containers.

### Key Design: Container Pooling
One pool container per dialect hosts multiple isolated databases (80x faster than container-per-instance):
- `InstanceManager` (`src/db/manager.rs`) creates databases inside pool containers via SQL commands
- Each instance gets unique credentials scoped to its database
- Uses `docker exec` with CLI tools (mysql, sqlcmd) instead of native drivers

### Core Flow
1. `POST /db/new` → `InstanceManager::create_instance()` → creates database in pool container
2. `POST /db/{id}/query` → `QueryExecutor::execute()` → `docker exec` with CLI tool → SSE stream
3. Inactive instances auto-archive (backup to R2) or destroy after 30 min

### Module Structure
- `src/api/` - Axum HTTP endpoints and SSE streaming
- `src/db/dialects/` - `Dialect` trait implementations (MySQL, SQL Server, Oracle)
- `src/db/manager.rs` - Instance lifecycle and pool container management
- `src/db/query.rs` - Query execution via docker exec
- `src/docker/` - Bollard wrapper for container operations
- `src/storage/` - SQLite metadata + R2 backup

### Adding a New Database Dialect
Implement the `Dialect` trait in `src/db/dialects/`:
- `docker_image()`, `default_port()`, `cli_command()`, `health_check_command()`
- Pool methods: `create_database_sql()`, `create_user_sql()`, `exec_sql_command()`
- Register in `get_dialect()` match

### Environment
Requires Docker socket access. Makefile auto-detects Colima vs Docker Desktop.
