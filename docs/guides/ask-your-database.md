# Ask Your Database — Complete AI Workflow

**What you'll build**: A security incident database where you can ask questions in natural language and get answers backed by evidence. By the end of this tutorial, you will insert incident records, search them by context, run semantic similarity queries, and ask the database plain-English questions that return grounded, source-cited answers.

**What you'll learn**:
- Configuring an AI provider (environment variable or vault)
- Creating tables with context indexes
- Auto-embedding text fields on insert
- Searching by context across every data model
- Semantic similarity search without managing vectors yourself
- Asking natural-language questions with the `ASK` command
- Polling real-time changes via CDC

**Time estimate**: 10 minutes

**Prerequisites**:
- [ ] RedDB installed ([installation guide](/getting-started/installation.md))
- [ ] An API key from at least one AI provider (Groq, OpenAI, Anthropic, etc.) &mdash; [Groq is free](https://console.groq.com)
- [ ] `curl` available in your terminal

---

## Step 1: Start RedDB

Create a fresh database for this tutorial. The `--http-bind` flag starts the HTTP server on the given address.

```bash
red server --path ./data/incidents.rdb --http-bind 127.0.0.1:8080
```

Verify the server is running:

```bash
curl -s http://127.0.0.1:8080/health
```

You should see:

```json
{"ok":true}
```

---

## Step 2: Configure Your AI Provider

RedDB needs an API key to call an LLM. You have two options.

### Option A: Environment variable (simplest)

Export the key before starting the server. Replace `gsk_your_key_here` with your actual Groq key.

```bash
export REDDB_GROQ_API_KEY=gsk_your_key_here
```

Then restart the server so it picks up the variable. If you prefer a different provider, the naming convention is `REDDB_{PROVIDER}_API_KEY` (e.g., `REDDB_OPENAI_API_KEY`, `REDDB_ANTHROPIC_API_KEY`).

### Option B: Store in the RedDB vault (persisted)

Store the key inside RedDB itself. The key is saved to the `red_config` collection and survives restarts.

```bash
curl -X POST http://127.0.0.1:8080/ai/credentials \
  -H 'content-type: application/json' \
  -d '{
    "provider": "groq",
    "api_key": "gsk_your_key_here",
    "default": true,
    "model": "llama-3.3-70b-versatile"
  }'
```

The `"default": true` flag tells RedDB to use Groq whenever you omit `USING` from a query.

### Verify configuration

```bash
curl -s http://127.0.0.1:8080/config/red.ai.default
```

Expected response:

```json
{
  "ok": true,
  "key": "red.ai.default",
  "value": {
    "provider": "groq",
    "model": "llama-3.3-70b-versatile"
  }
}
```

---

## Step 3: Create the Incidents Table

Create a table with a context index on `host` and `severity`. The context index lets RedDB find related data across all data models when you search by those fields.

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "CREATE TABLE incidents (title TEXT, severity TEXT, host TEXT, description TEXT) WITH TTL 90 d WITH CONTEXT INDEX ON (host, severity)"}'
```

Expected response:

```json
{"ok":true,"message":"table created","table":"incidents"}
```

---

## Step 4: Insert Sample Incidents

Populate the table with realistic security incidents. Each `curl` command is self-contained and copy-pasteable.

**Incident 1 — SSH Brute Force**

```bash
curl -X POST http://127.0.0.1:8080/collections/incidents/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "title": "SSH Brute Force",
      "severity": "high",
      "host": "10.0.0.5",
      "description": "Multiple failed SSH login attempts from 192.168.1.100. 500 attempts in 5 minutes targeting root account."
    }
  }'
```

**Incident 2 — Malware Detected**

```bash
curl -X POST http://127.0.0.1:8080/collections/incidents/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "title": "Malware Detected",
      "severity": "critical",
      "host": "10.0.0.5",
      "description": "Trojan.GenericKD detected in /tmp/payload.exe by endpoint agent. Process attempted outbound connection to 203.0.113.50:4444."
    }
  }'
```

**Incident 3 — Privilege Escalation**

```bash
curl -X POST http://127.0.0.1:8080/collections/incidents/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "title": "Privilege Escalation",
      "severity": "critical",
      "host": "10.0.0.12",
      "description": "User jdoe escalated to root via sudo exploit CVE-2023-22809. Unauthorized crontab entry created."
    }
  }'
```

**Incident 4 — Port Scan Detected**

```bash
curl -X POST http://127.0.0.1:8080/collections/incidents/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "title": "Port Scan Detected",
      "severity": "medium",
      "host": "10.0.0.20",
      "description": "Nmap SYN scan detected from 192.168.1.55 targeting ports 1-1024 on host 10.0.0.20. 47 open ports identified."
    }
  }'
```

**Incident 5 — Data Exfiltration Attempt**

```bash
curl -X POST http://127.0.0.1:8080/collections/incidents/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "title": "Data Exfiltration Attempt",
      "severity": "critical",
      "host": "10.0.0.5",
      "description": "Unusual outbound traffic spike detected. 2.3 GB transferred to external IP 198.51.100.77 over DNS tunnel in 30 minutes."
    }
  }'
```

Verify the data is in:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT title, severity, host FROM incidents"}'
```

Expected output (5 rows):

```json
{
  "ok": true,
  "columns": ["title", "severity", "host"],
  "rows": [
    ["SSH Brute Force", "high", "10.0.0.5"],
    ["Malware Detected", "critical", "10.0.0.5"],
    ["Privilege Escalation", "critical", "10.0.0.12"],
    ["Port Scan Detected", "medium", "10.0.0.20"],
    ["Data Exfiltration Attempt", "critical", "10.0.0.5"]
  ],
  "count": 5
}
```

---

## Step 5: Auto-Embed Descriptions for Semantic Search

Use `WITH AUTO EMBED` to insert a record and simultaneously generate a vector embedding for the `description` field. RedDB calls your AI provider, stores the embedding, and links it to the row automatically.

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "INSERT INTO incidents (title, severity, host, description) VALUES ('\''Ransomware Encryption'\'', '\''critical'\'', '\''10.0.0.12'\'', '\''BitLocker encryption triggered on all NTFS volumes. Ransom note dropped at C:\\README_DECRYPT.txt. Kill chain matches LockBit 3.0 playbook.'\'') WITH AUTO EMBED (description) USING groq"}'
```

This single command does three things:

1. Inserts the row into the `incidents` table
2. Calls the Groq embedding API with the `description` text
3. Stores the resulting vector in the `incidents` vector index

> **Tip**: You can use `WITH AUTO EMBED` on every insert. For bulk historical data, the `/ai/embeddings` endpoint with `source_query` mode is more efficient. See the [HTTP API docs](/api/http.md#embeddings).

---

## Step 6: Search by Context

Context search finds everything related to a value across all data models &mdash; tables, graphs, vectors, documents, and key-values &mdash; in a single request.

Find all incidents related to host `10.0.0.5`:

```bash
curl -X POST http://127.0.0.1:8080/context \
  -H 'content-type: application/json' \
  -d '{
    "query": "10.0.0.5",
    "field": "host"
  }'
```

Expected response (abbreviated):

```json
{
  "ok": true,
  "tables": [
    {
      "_entity_id": 1,
      "_collection": "incidents",
      "title": "SSH Brute Force",
      "severity": "high",
      "host": "10.0.0.5",
      "description": "Multiple failed SSH login attempts from 192.168.1.100..."
    },
    {
      "_entity_id": 2,
      "_collection": "incidents",
      "title": "Malware Detected",
      "severity": "critical",
      "host": "10.0.0.5",
      "description": "Trojan.GenericKD detected in /tmp/payload.exe..."
    },
    {
      "_entity_id": 5,
      "_collection": "incidents",
      "title": "Data Exfiltration Attempt",
      "severity": "critical",
      "host": "10.0.0.5",
      "description": "Unusual outbound traffic spike detected..."
    }
  ],
  "graph": { "nodes": [], "edges": [] },
  "vectors": [],
  "documents": [],
  "key_values": [],
  "connections": [],
  "summary": {
    "total_hits": 3,
    "tables": 3,
    "graph_nodes": 0,
    "graph_edges": 0,
    "vectors": 0,
    "documents": 0,
    "key_values": 0
  }
}
```

Three incidents share host `10.0.0.5`. The context search found them all through the context index on the `host` field. If you had graph edges or documents referencing that IP, they would appear here too.

Or use SQL directly:

```sql
SEARCH CONTEXT '10.0.0.5' FIELD host DEPTH 2 LIMIT 50
```

---

## Step 7: Semantic Search

Semantic search finds records by meaning, not exact keywords. Search for "unauthorized access attempt" &mdash; even though no record contains that exact phrase.

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SEARCH SIMILAR TEXT '\''unauthorized access attempt'\'' COLLECTION incidents LIMIT 5 USING groq"}'
```

Expected response:

```json
{
  "ok": true,
  "results": [
    {
      "_entity_id": 1,
      "_collection": "incidents",
      "_kind": "vector",
      "_score": 0.87,
      "content": "Multiple failed SSH login attempts from 192.168.1.100. 500 attempts in 5 minutes targeting root account."
    },
    {
      "_entity_id": 3,
      "_collection": "incidents",
      "_kind": "vector",
      "_score": 0.82,
      "content": "User jdoe escalated to root via sudo exploit CVE-2023-22809. Unauthorized crontab entry created."
    }
  ]
}
```

The SSH brute force and privilege escalation incidents matched, even though neither contains the words "unauthorized access attempt." The embedding model understood the semantic relationship. Scores are approximate and depend on the provider and model.

---

## Step 8: Ask a Question

This is the headline feature. `ASK` combines context retrieval with LLM synthesis in a single command.

### Via HTTP

```bash
curl -X POST http://127.0.0.1:8080/ai/ask \
  -H 'content-type: application/json' \
  -d '{
    "question": "What happened on host 10.0.0.5 and how severe is it?",
    "provider": "groq",
    "model": "llama-3.3-70b-versatile"
  }'
```

Expected response:

```json
{
  "ok": true,
  "answer": "Host 10.0.0.5 experienced three security incidents of escalating severity:\n\n1. **SSH Brute Force (high)**: 500 failed SSH login attempts from 192.168.1.100 in 5 minutes, targeting the root account.\n2. **Malware Detected (critical)**: Trojan.GenericKD found in /tmp/payload.exe. The process attempted outbound communication to 203.0.113.50:4444, indicating possible C2 activity.\n3. **Data Exfiltration Attempt (critical)**: 2.3 GB transferred to 198.51.100.77 via DNS tunnel in 30 minutes.\n\nThis host is severely compromised. The pattern suggests an attacker gained initial access via SSH brute force, deployed malware with C2 capability, and began exfiltrating data. Immediate isolation and forensic analysis are recommended.",
  "provider": "groq",
  "model": "llama-3.3-70b-versatile",
  "prompt_tokens": 1847,
  "completion_tokens": 195,
  "sources": {
    "tables": [ "..." ],
    "graph": { "nodes": [], "edges": [] },
    "vectors": [],
    "documents": [],
    "key_values": [],
    "connections": [],
    "summary": { "total_hits": 3, "tables": 3 }
  }
}
```

The response contains:

| Field | Description |
|:------|:------------|
| `answer` | Natural-language answer grounded in your data |
| `sources` | The context search results the LLM used as evidence |
| `provider` | Which AI provider generated the answer |
| `model` | Which model was used |
| `prompt_tokens` | Tokens consumed by the context + question |
| `completion_tokens` | Tokens in the generated answer |

### Via SQL

You can also ask questions directly in SQL. If you set a default provider in Step 2, you can omit `USING`:

```sql
ASK 'which hosts had critical incidents and what should we investigate first?'
```

Or specify the provider and model inline:

```sql
ASK 'what is the most likely attack chain across all hosts?' USING groq MODEL 'llama-3.3-70b-versatile' DEPTH 2
```

Run it through the query endpoint:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "ASK '\''which hosts had critical incidents and what should we investigate first?'\'' USING groq"}'
```

---

## Step 9: Monitor Changes in Real-Time

Every insert, update, and delete emits a change event. Poll the CDC endpoint to track what happened since your last checkpoint.

```bash
curl -s 'http://127.0.0.1:8080/changes?since_lsn=0&limit=10'
```

Expected response:

```json
{
  "ok": true,
  "events": [
    {
      "lsn": 1,
      "timestamp": 1744329600000,
      "operation": "insert",
      "collection": "incidents",
      "entity_id": 1,
      "entity_kind": "table"
    },
    {
      "lsn": 2,
      "timestamp": 1744329600100,
      "operation": "insert",
      "collection": "incidents",
      "entity_id": 2,
      "entity_kind": "table"
    }
  ],
  "next_lsn": 7
}
```

Use `next_lsn` as your cursor for the next poll:

```bash
curl -s 'http://127.0.0.1:8080/changes?since_lsn=7&limit=10'
```

This gives you a real-time audit trail. Build alerting pipelines, sync to external systems, or trigger automated analysis on every new incident.

---

## What's Happening Under the Hood

Here is what each command does internally, so you understand the machinery behind the one-liners.

### ASK = Context Search + LLM Synthesis

```
ASK 'question'
  ├─ SEARCH CONTEXT 'question'        ← find relevant data across all models
  │    ├─ Field-value index lookup     ← O(1) if a context index exists
  │    ├─ Token index scan             ← fallback: tokenized keyword matching
  │    ├─ Global scan                  ← last resort: scan all collections
  │    ├─ Graph expansion              ← follow edges from matched nodes
  │    └─ Cross-reference resolution   ← follow links between entities
  ├─ Build prompt with context JSON    ← serialize results as LLM context
  └─ Call LLM provider                 ← send to Groq/OpenAI/Anthropic/etc.
```

### SEARCH CONTEXT = 3-Tier Strategy

The context search uses three tiers, stopping when it has enough results:

1. **Field-value index** &mdash; exact match on context-indexed fields (`O(1)`)
2. **Token index** &mdash; tokenized keyword matching across all fields
3. **Global scan** &mdash; full scan as a last resort

After matching, it expands results by traversing graph edges and following cross-references between entities.

### AUTO EMBED = Insert + Embed + Store

`WITH AUTO EMBED (field)` on an INSERT:

1. Inserts the row into the table
2. Extracts the specified field values
3. Calls the embedding API (Groq, OpenAI, etc.)
4. Stores the resulting vector linked to the row

### CDC = Change Event Stream

Every write operation (INSERT, UPDATE, DELETE) appends an event to the CDC buffer with a monotonically increasing LSN (Log Sequence Number). Consumers poll `/changes?since_lsn=N` to read events incrementally.

---

## Next Steps

- **[AI Providers](/api/http.md#ai)** &mdash; Configure multiple providers, set aliases, rotate keys
- **[Graph Analytics](/guides/graph-analytics.md)** &mdash; Build network graphs from your incidents and run PageRank, community detection, and shortest-path queries
- **[Vector Search App](/guides/vector-search.md)** &mdash; Deep dive into HNSW, IVF, and hybrid search
- **[Backup & Recovery](/api/http.md#backup--recovery)** &mdash; Enable scheduled backups and WAL archiving
- **[Configuration Reference](/getting-started/configuration.md)** &mdash; Full list of `red.*` config keys for tuning AI, search, and storage behavior
