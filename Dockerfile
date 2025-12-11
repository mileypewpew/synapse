# Stage 1: Builder
FROM rust:1.83-alpine AS builder

WORKDIR /app

# Install build dependencies
RUN apk add --no-cache musl-dev

# Copy dependency files first for caching
COPY Cargo.toml Cargo.lock ./

# Create dummy sources to build dependencies
RUN mkdir -p src/bin && \
    echo "fn main() {}" > src/main.rs && \
    echo "fn main() {}" > src/bin/worker.rs

# Build dependencies only
RUN cargo build --release

# Copy actual source code
COPY src ./src

# Touch files to force rebuild of app (not deps)
RUN touch src/main.rs src/bin/worker.rs

# Build the actual binaries
RUN cargo build --release

# Stage 2: Runtime (Distroless or Alpine)
# Using Alpine for easier debugging (shell access) in prototype phase
FROM alpine:3.19

WORKDIR /app

# Install runtime dependencies (ssl certificates)
RUN apk add --no-cache ca-certificates tzdata

# Copy binaries from builder
COPY --from=builder /app/target/release/synapse /app/server
COPY --from=builder /app/target/release/worker /app/worker

# Expose API port
EXPOSE 3000

# Default command (can be overridden to run worker)
CMD ["/app/server"]
