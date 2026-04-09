# Building a REST API with RedDB

This guide shows how to build a REST API backed by RedDB.

## Setup

Start the RedDB HTTP server:

```bash
red server --http --path ./data/app.rdb --bind 127.0.0.1:8080
```

## Create Your Data Model

```bash
# Create a users collection with some initial data
curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Alice", "email": "alice@example.com", "role": "admin"}}'

curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Bob", "email": "bob@example.com", "role": "user"}}'
```

## CRUD Operations

### List (with pagination)

```bash
curl "http://127.0.0.1:8080/collections/users/scan?offset=0&limit=20"
```

### Search (with filters)

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM users WHERE role = '\''admin'\'' ORDER BY name LIMIT 20"}'
```

### Get by ID

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM users WHERE _entity_id = 1"}'
```

### Create

```bash
curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Charlie", "email": "charlie@example.com", "role": "user"}}'
```

### Update

```bash
curl -X PATCH http://127.0.0.1:8080/collections/users/entities/1 \
  -H 'content-type: application/json' \
  -d '{"fields": {"role": "superadmin"}}'
```

### Delete

```bash
curl -X DELETE http://127.0.0.1:8080/collections/users/entities/3
```

## Adding Related Data

Link users to projects using graph edges:

```bash
# Create a project node
curl -X POST http://127.0.0.1:8080/collections/projects/nodes \
  -H 'content-type: application/json' \
  -d '{"label": "reddb", "node_type": "project", "properties": {"name": "RedDB", "status": "active"}}'

# Link user to project
curl -X POST http://127.0.0.1:8080/collections/projects/edges \
  -H 'content-type: application/json' \
  -d '{"label": "MEMBER_OF", "from": 1, "to": 3, "properties": {"role": "maintainer"}}'
```

## Query Across Models

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "FROM ANY WHERE _collection = '\''users'\'' OR _collection = '\''projects'\'' ORDER BY _score DESC LIMIT 50"}'
```

## Enable Auth

For production, enable authentication:

```bash
red server --http --path ./data/app.rdb --vault --bind 0.0.0.0:8080
```

Then bootstrap and create API keys for your services. See [Auth & Security](/security/overview.md).
