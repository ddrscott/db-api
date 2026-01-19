# Build stage
FROM rust:1.88-slim as builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create dummy main.rs to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release && rm -rf src

# Copy actual source
COPY src ./src

# Build the real binary (touch to force rebuild)
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies and Litestream
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Install Litestream
ARG LITESTREAM_VERSION=0.3.13
RUN curl -fsSL "https://github.com/benbjohnson/litestream/releases/download/v${LITESTREAM_VERSION}/litestream-v${LITESTREAM_VERSION}-linux-amd64.tar.gz" \
    | tar -xz -C /usr/local/bin

# Create data directory for SQLite
RUN mkdir -p /data

# Copy binary from builder
COPY --from=builder /app/target/release/db-api /app/db-api

# Copy Litestream config
COPY litestream.yml /etc/litestream.yml

EXPOSE 8013

ENV HOST=0.0.0.0
ENV PORT=8013
ENV RUST_LOG=info
ENV METADATA_DB_PATH=/data/metadata.db

# Start with Litestream replication wrapping the app
# Litestream will restore the database on startup and replicate changes continuously
CMD ["litestream", "replicate", "-exec", "/app/db-api", "-config", "/etc/litestream.yml"]
