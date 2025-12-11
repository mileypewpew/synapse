# WASD Synapse

**W**orkers | **A**dapters | **S**ynapse | **D**ispatcher

*Universal Event Processing Engine*

## Overview

Synapse is a high-performance, fault-tolerant engine designed to ingest, understand, and act upon events from any source. Built with Rust, Axum, and Redis Streams.

## Architecture

```
Client -> HTTP API -> Redis Stream -> Worker -> Router -> Effects
                           |              |
                           v              v
                    Consumer Groups   Discord/Webhooks
```

1. **Server (`src/main.rs`):** HTTP API for event ingestion with auth, metrics, and DLQ management
2. **Worker (`src/bin/worker.rs`):** Background process that consumes events via Consumer Groups (`XREADGROUP`)
3. **Router (`src/router.rs`):** Pattern-based event routing with wildcard support
4. **Effects:** Pluggable handlers (Log, Webhook with Discord embeds)

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

**Metrics:**
```bash
curl http://localhost:3000/metrics
# {"version":"0.1.0","uptime_seconds":123,"uptime":"2m 3s","events":{"received":42},"status":"running"}
```

**Emit Event:**
```bash
curl -X POST http://localhost:3000/api/v1/events \
  -H "Authorization: Bearer dev-key" \
  -H "Content-Type: application/json" \
  -H "X-Correlation-ID: my-trace-id" \
  -d '{
    "source": "minecraft",
    "eventType": "player.joined",
    "payload": {"player": "Steve", "server": "haumcraft"}
  }'
# {"id":"1702300000000-0","status":"accepted","correlationId":"my-trace-id"}
```

**List Dead Letter Queue:**
```bash
curl http://localhost:3000/api/v1/dlq \
  -H "Authorization: Bearer dev-key"
# {"total":0,"limit":50,"offset":0,"entries":[]}
```

**Get DLQ Entry:**
```bash
curl http://localhost:3000/api/v1/dlq/{id} \
  -H "Authorization: Bearer dev-key"
```

**Remove DLQ Entry:**
```bash
curl -X DELETE http://localhost:3000/api/v1/dlq/{id} \
  -H "Authorization: Bearer dev-key"
```

## Configuration

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `REDIS_URL` | Redis connection string | *required* |
| `SYNAPSE_API_KEY` | Bearer token for API authentication | `dev-key` |
| `PORT` | Server listen port | `3000` |
| `RUST_LOG` | Logging level | `info` |
| `SYNAPSE_CONFIG` | Path to config file | `synapse.toml` |
| `SYNAPSE_WORKER_NAME` | Worker identifier | hostname or UUID |
| `SYNAPSE_CONSUMER_GROUP` | Consumer group name | `synapse_workers` |

### Configuration File (synapse.toml)

```toml
[server]
port = 3000
api_key = "${SYNAPSE_API_KEY}"

[redis]
url = "${REDIS_URL}"

[worker]
name = "${SYNAPSE_WORKER_NAME}"
consumer_group = "synapse_workers"

# Event routing rules
[routes]
"user.*" = ["log:user"]
"game.*" = ["log:game", "webhook:discord"]
"player.*" = ["log:game", "webhook:discord"]
"system.*" = ["log:system"]
"*" = ["log:default"]  # catch-all

# Effect configurations
[effects.log.user]
prefix = "user"

[effects.log.game]
prefix = "game"

[effects.log.system]
prefix = "system"

[effects.log.default]
prefix = "unhandled"

[effects.webhook.discord]
url = "${DISCORD_WEBHOOK_URL}"
timeout_ms = 10000
retries = 2
format = "discord"
```

## Features

### Event Routing

- **Exact match:** `"user.created"` matches only `user.created`
- **Wildcard:** `"game.*"` matches `game.started`, `game.achievement`, etc.
- **Catch-all:** `"*"` matches all events (default handler)

### Discord Integration

Events routed to Discord webhooks are formatted as rich embeds:

```json
{
  "embeds": [{
    "title": "Game Achievement",
    "description": "**Steve** unlocked **Diamonds!**",
    "color": 5763719,
    "fields": [
      {"name": "Player", "value": "Steve", "inline": true},
      {"name": "Server", "value": "haumcraft", "inline": true}
    ],
    "timestamp": "2025-12-11T10:00:00Z",
    "footer": {"text": "Source: minecraft"}
  }]
}
```

### Event Metadata

Events include automatic metadata:
- `timestamp`: ISO 8601 timestamp (server-assigned)
- `correlationId`: From `X-Correlation-ID` header or auto-generated UUID

### Reliability Features

- **Graceful shutdown:** Clean termination on SIGTERM/SIGINT
- **Consumer groups:** At-least-once delivery with Redis Streams
- **Pending recovery:** Workers claim idle messages on startup (XAUTOCLAIM)
- **Exponential backoff:** Retries with 1s, 2s, 4s delays
- **Dead Letter Queue:** Failed events after 3 retries move to DLQ

### Observability

- `/health` - Service health and Redis connectivity
- `/metrics` - Uptime, version, events received
- `/api/v1/dlq` - Dead Letter Queue management

## Development

### Local Development

```bash
# Start Redis
docker run -d --name redis -p 6379:6379 redis:alpine

# Set environment
export REDIS_URL="redis://localhost:6379"
export SYNAPSE_API_KEY="dev-key"
export RUST_LOG="debug"

# Run server
cargo run

# Run worker (separate terminal)
cargo run --bin worker
```

### Testing

```bash
cargo test
cargo clippy
cargo fmt --check
```

### CI/CD

GitHub Actions workflow runs on every push:
- Format check (`cargo fmt`)
- Linting (`cargo clippy`)
- Tests (`cargo test`)
- Docker build (main branch only)

## Project Structure

```
src/
├── main.rs           # HTTP server, API endpoints
├── lib.rs            # Library exports
├── bin/
│   └── worker.rs     # Event processing worker
├── config.rs         # TOML configuration
├── event.rs          # Event types
├── router.rs         # Pattern routing
├── effects/          # Effect handlers
│   ├── mod.rs        # Effect trait
│   ├── log.rs        # Logging effect
│   └── webhook.rs    # Webhook/Discord effect
├── shutdown.rs       # Graceful shutdown
└── dlq.rs            # Dead Letter Queue

config/
└── synapse.toml      # Example configuration

.github/
└── workflows/
    └── ci.yml        # CI/CD pipeline
```

## Version History

- **v0.3.0** - Sprint 5: Configuration, wildcards, Discord, metadata, metrics
- **v0.4.0** - Sprint 6: Graceful shutdown, resilience, DLQ, CI/CD
