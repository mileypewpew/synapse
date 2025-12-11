# Synapse -> WASDFX Activity Feed Integration

This document describes the integration between Synapse (Event Engine) and the WASDFX Activity Feed API.

## Overview

**Synapse** is a lightweight event routing engine that receives events from game servers (e.g., Minecraft) and routes them to various destinations. One of these destinations is the WASDFX Activity Feed.

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Minecraft  │────▶│   Synapse   │────▶│   WASDFX    │
│   Server    │     │   (Router)  │     │ Activity API│
└─────────────┘     └─────────────┘     └─────────────┘
```

When game events occur (player joins, achievements, etc.), Synapse transforms them into the WASDFX Activity Feed format and posts them to the API.

---

## API Contract

### Endpoint Used

```
POST /api/v1/events/emit
```

### Authentication

Synapse authenticates using the `X-API-Key` header:

```http
POST /api/v1/events/emit HTTP/1.1
Host: localhost:4000
Content-Type: application/json
X-API-Key: sk_test_dev_key
```

**Required Configuration**: The API key must be present in WASDFX's `API_KEYS` environment variable.

### Request Payload

Synapse sends payloads matching this structure:

```json
{
  "type": "ACHIEVEMENT_UNLOCKED",
  "title": "Steve unlocked an achievement!",
  "content": "**Steve** unlocked **Diamonds**!",
  "iconType": "achievement",
  "serverId": "cmi55214n0000oykqes3x5u27",
  "category": "game",
  "priority": 0,
  "metadata": {
    "synapse_source": "minecraft",
    "synapse_event_type": "game.achievement",
    "synapse_correlation_id": "uuid-here",
    "original_payload": {
      "player": "Steve",
      "achievement": "Diamonds"
    }
  }
}
```

### Expected Response

**Success (201 Created)**:
```json
{
  "success": true,
  "event": {
    "id": "cmj1kpxhz0007oyrs4va4c2t0",
    "type": "ACHIEVEMENT_UNLOCKED",
    "title": "Steve unlocked an achievement!",
    "createdAt": "2025-12-11T15:09:02.815Z"
  }
}
```

**Error Responses**:
- `401 Unauthorized` - Invalid or missing API key
- `400 Bad Request` - Validation failed (missing required fields)
- `500 Internal Server Error` - Database or server error

---

## Event Type Mappings

Synapse maps game events to WASDFX ActivityTypes:

| Synapse Event Type | WASDFX ActivityType | Icon Type |
|--------------------|---------------------|-----------|
| `game.achievement` | `ACHIEVEMENT_UNLOCKED` | `achievement` |
| `player.achievement` | `ACHIEVEMENT_UNLOCKED` | `achievement` |
| `player.joined` | `PLAYER_JOINED` | `success` |
| `player.left` | `PLAYER_LEFT` | `info` |
| `game.milestone` | `MILESTONE_REACHED` | `achievement` |
| `system.alert` | `SERVER_ANNOUNCEMENT` | `warning` |
| `system.announcement` | `SERVER_ANNOUNCEMENT` | `info` |
| *(any other)* | `SERVER_ANNOUNCEMENT` | `info` |

---

## Content Formatting

Synapse generates human-readable titles and markdown-formatted content:

### Achievement Events
- **Title**: `"{player} unlocked an achievement!"`
- **Content**: `"**{player}** unlocked **{achievement}**!"`

### Player Join Events
- **Title**: `"{player} joined the server"`
- **Content**: `"**{player}** has joined **{server}**"`

### Player Leave Events
- **Title**: `"{player} left the server"`
- **Content**: `"**{player}** has left the server"`

### Other Events
- **Title**: Event type converted to title case (e.g., `game.start` → `"Game Start"`)
- **Content**: `message` field from payload, or JSON-formatted payload

---

## Metadata

Every event includes a `metadata` object with tracing information:

```json
{
  "metadata": {
    "synapse_source": "minecraft",
    "synapse_event_type": "game.achievement",
    "synapse_correlation_id": "e29c1fe1-0f25-4ed7-879f-f496fe37a4ce",
    "original_payload": { ... }
  }
}
```

This allows:
- Tracing events back to their source
- Debugging event transformations
- Accessing original payload data if needed

---

## Configuration (WASDFX Side)

### Required Environment Variables

```bash
# API key that Synapse will use (in apps/api/.env.local or .env)
API_KEYS=sk_test_dev_key

# For production, use a secure key:
# API_KEYS=sk_prod_<random-32-bytes-hex>
```

### Server ID

Synapse is configured with a specific `serverId` to associate events with. This must be a valid server ID from the WASDFX database:

```sql
SELECT id, name, slug FROM servers;
-- Example: cmi55214n0000oykqes3x5u27 | Haumcraft Season VI | haumcraft-season-vi
```

---

## Testing the Integration

### Send a Test Event via Synapse

```bash
curl -X POST http://localhost:3000/api/v1/events \
  -H "Authorization: Bearer dev-key" \
  -H "Content-Type: application/json" \
  -d '{
    "source": "minecraft",
    "eventType": "game.achievement",
    "payload": {
      "player": "TestPlayer",
      "achievement": "Test Achievement"
    }
  }'
```

### Verify in WASDFX Database

```sql
SELECT type, title, content, metadata
FROM activity_feed_items
ORDER BY "createdAt" DESC
LIMIT 1;
```

### Send Directly to WASDFX API (bypass Synapse)

```bash
curl -X POST http://localhost:4000/api/v1/events/emit \
  -H "X-API-Key: sk_test_dev_key" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "ACHIEVEMENT_UNLOCKED",
    "title": "Test Achievement",
    "content": "This is a test",
    "category": "game",
    "serverId": "cmi55214n0000oykqes3x5u27"
  }'
```

---

## Future Considerations

### 1. New ActivityTypes
If WASDFX adds new ActivityTypes that should map to game events, the Synapse team needs to update `src/effects/wasdfx.rs`:

```rust
fn map_activity_type(event_type: &str) -> &'static str {
    match event_type {
        "game.achievement" => "ACHIEVEMENT_UNLOCKED",
        // Add new mappings here
        _ => "SERVER_ANNOUNCEMENT",
    }
}
```

### 2. Rate Limiting
Consider implementing rate limiting on the WASDFX API if high event volume becomes an issue. Minecraft servers can generate many events during peak times.

### 3. Batch Events
For high-throughput scenarios, a batch endpoint could be more efficient:
```
POST /api/v1/events/emit/batch
```

### 4. Server Slug Support
Currently requires database ID. A slug-based lookup would be more user-friendly:
```json
{ "serverSlug": "haumcraft-season-vi" }
```

---

## Contact

- **Synapse Repository**: [github.com/mileypewpew/synapse](https://github.com/mileypewpew/synapse)
- **Integration Added**: December 2025
