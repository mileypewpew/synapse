# WASD Synapse

**W**orkers | **A**dapters | **S**ynapse | **D**ispatcher

*Universal Event Processing Engine*

## Overview
Synapse is a high-performance, fault-tolerant engine designed to ingest, understand, and act upon events from any source. It is built with Rust, Axum, and Redis Streams.

## Architecture
1.  **Server (`src/main.rs`):** HTTP API for event ingestion. Pushes events to Redis Stream.
2.  **Worker (`src/bin/worker.rs`):** Background process that consumes events via Consumer Groups (`XREADGROUP`) and processes them.

## Getting Started

### Prerequisites
- Docker & Docker Compose
- Rust (optional, for local dev)

### Running with Docker (Recommended)
```bash
docker-compose up -d --build
```

This starts:
- **Redis:** Exposed on port `6380`
- **Synapse Server:** Exposed on port `3000`
- **Synapse Worker:** Background processing

### API Usage

**Health Check:**
```bash
curl http://localhost:3000/health
# {"status":"ok","redis":"connected"}
```

**Emit Event:**
```bash
curl -X POST http://localhost:3000/api/v1/events \
  -H "Authorization: Bearer dev-key" \
  -H "Content-Type: application/json" \
  -d '{"source": "test", "eventType": "user.action", "payload": {"foo": "bar"}}'
```

## Configuration
Configuration is handled via environment variables (see `docker-compose.yml`):
- `REDIS_URL`: Connection string for Redis.
- `SYNAPSE_API_KEY`: Bearer token for API authentication.
- `RUST_LOG`: Logging level (e.g., `info`, `debug`).
