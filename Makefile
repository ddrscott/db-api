# db-api Makefile

# Detect Docker socket (Colima vs Docker Desktop)
DOCKER_SOCKET := $(shell \
	if [ -S "$$HOME/.colima/default/docker.sock" ]; then \
		echo "unix://$$HOME/.colima/default/docker.sock"; \
	elif [ -S "/var/run/docker.sock" ]; then \
		echo "unix:///var/run/docker.sock"; \
	else \
		echo ""; \
	fi)

# Configuration
PORT ?= 8081
RUST_LOG ?= info

# Export for subprocesses
export DOCKER_HOST := $(DOCKER_SOCKET)
export PORT
export RUST_LOG

.PHONY: run build release check clean test docker-build docker-run help

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

run: ## Run the server (debug mode)
	@echo "Using Docker socket: $(DOCKER_SOCKET)"
	@echo "Server will listen on port $(PORT)"
	cargo run

build: ## Build debug binary
	cargo build

release: ## Build release binary
	cargo build --release

check: ## Check code without building
	cargo check

clean: ## Clean build artifacts
	cargo clean

test: ## Run tests
	cargo test

fmt: ## Format code
	cargo fmt

lint: ## Run clippy linter
	cargo clippy -- -W warnings

docker-build: ## Build Docker image
	docker build -t db-api .

docker-run: docker-build ## Run in Docker container
	docker run -p $(PORT):$(PORT) \
		-v /var/run/docker.sock:/var/run/docker.sock \
		-e PORT=$(PORT) \
		-e RUST_LOG=$(RUST_LOG) \
		db-api

# Quick test targets
test-health: ## Test health endpoint
	curl -s http://localhost:$(PORT)/health | jq .

test-mysql: ## Create a MySQL instance
	curl -s -X POST http://localhost:$(PORT)/db/new \
		-H "Content-Type: application/json" \
		-d '{"dialect": "mysql"}' | jq .
