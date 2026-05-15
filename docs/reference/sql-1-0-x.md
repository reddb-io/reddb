# What Works in RedDB 1.0.x SQL

This page is the compact reference for the SQL/RQL surface shipped in the
1.0.x and 1.1.x-compatible line. It lists accepted syntax, a minimal example,
and the current support status. Prefer parameterized `$N` values for user input.

Generated blocks are checked by `scripts/check-sql-reference.py`.

## Generated Parser Surface

Lexer keywords:

<!-- generated:lexer-keywords begin -->
`ACK`, `ADD`, `ALGORITHM`, `ALL`, `ALTER`, `ANALYZE`, `AND`, `AS`, `ASC`, `ATTACH`, `AVG`, `BEGIN`, `BETWEEN`, `BY`, `CASCADE`, `CENTRALITY`, `CLUSTERING`, `COLLECTION`, `COLUMN`, `COMMIT`, `COMMUNITY`, `COMPONENTS`, `COMPRESS`, `CONTAINS`, `COPY`, `COSINE`, `COUNT`, `CREATE`, `CROSS`, `CYCLES`, `DATA`, `DEFAULT`, `DELETE`, `DELIMITER`, `DEPTH`, `DESC`, `DETACH`, `DIRECTION`, `DISABLE`, `DISTINCT`, `DOCUMENT`, `DROP`, `EDGE`, `ENABLE`, `ENDS`, `ENRICH`, `EXISTS`, `EXPLAIN`, `FALSE`, `FIRST`, `FOR`, `FOREIGN`, `FORMAT`, `FROM`, `FULL`, `FUSION`, `FUZZY`, `GRAPH`, `GROUP`, `HASH`, `HEADER`, `HYBRID`, `IF`, `IN`, `INCLUDE`, `INCREMENT`, `INDEX`, `INNER`, `INNERPRODUCT`, `INNER_PRODUCT`, `INSERT`, `INTERSECTION`, `INTO`, `IS`, `JOIN`, `JSON`, `K`, `KEY`, `KV`, `L2`, `LAST`, `LEFT`, `LEVEL`, `LIKE`, `LIMIT`, `LIST`, `LPOP`, `LPUSH`, `MATCH`, `MATERIALIZED`, `MAX`, `MAXITERATIONS`, `MAXLENGTH`, `MAX_ITERATIONS`, `MAX_LENGTH`, `METADATA`, `METRIC`, `MIN`, `MINSCORE`, `MIN_SCORE`, `MODE`, `NACK`, `NEIGHBORHOOD`, `NODE`, `NOT`, `NULL`, `NULLS`, `OF`, `OFFSET`, `ON`, `OPTIONS`, `OR`, `ORDER`, `OUTER`, `PARTITION`, `PATH`, `PEEK`, `POLICY`, `POP`, `PRIMARY`, `PRIORITY`, `PROPERTIES`, `PURGE`, `PUSH`, `QUEUE`, `RANGE`, `RECURSIVE`, `REFRESH`, `RELEASE`, `RENAME`, `RERANK`, `RETENTION`, `RETURN`, `RETURNING`, `RIGHT`, `ROLLBACK`, `ROW`, `RPOP`, `RPUSH`, `RRF`, `SAVEPOINT`, `SCHEMA`, `SEARCH`, `SECURITY`, `SELECT`, `SEQUENCE`, `SERVER`, `SET`, `SHORTESTPATH`, `SHORTEST_PATH`, `SIMILAR`, `START`, `STARTS`, `STRATEGY`, `SUM`, `TABLE`, `TEXT`, `THRESHOLD`, `TIMESERIES`, `TO`, `TOPOLOGICALSORT`, `TOPOLOGICAL_SORT`, `TRANSACTION`, `TRAVERSE`, `TREE`, `TRUE`, `TRUNCATE`, `UNION`, `UNIQUE`, `UPDATE`, `USING`, `VACUUM`, `VALUES`, `VECTOR`, `VECTORS`, `VIA`, `VIEW`, `WEIGHT`, `WHERE`, `WITH`, `WORK`, `WRAPPER`
<!-- generated:lexer-keywords end -->

Top-level SQL dispatch:

<!-- generated:top-level-sql begin -->
`SELECT`, `FROM`, `INSERT`, `UPDATE`, `DELETE`, `EXPLAIN`, `CREATE`, `DROP`, `ALTER`, `SET`, `SHOW`, `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`, `START`, `VACUUM`, `ANALYZE`, `COPY`, `REFRESH`, `DESCRIBE`, `DESC`
<!-- generated:top-level-sql end -->

## Query Mode Detection

| Mode | Trigger | Example | Status |
| --- | --- | --- | --- |
| SQL/RQL | Starts with SQL/RQL commands such as `SELECT`, `FROM`, `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `DROP`, `ALTER`, `SHOW`, `DESCRIBE`, `GRAPH`, `SEARCH`, `QUEUE`, `KV`, `ASK`, `BEGIN` | `SELECT * FROM users` | supported |
| SPARQL | Starts with SPARQL forms such as `PREFIX`, `SELECT ... WHERE { ... }`, `ASK WHERE { ... }`, `CONSTRUCT`, `DESCRIBE` with SPARQL body | `SELECT ?s WHERE { ?s ?p ?o }` | partial |
| Cypher-like graph | Starts with `MATCH` and graph pattern syntax | `MATCH (a)-[:KNOWS]->(b) RETURN b` | partial |
| Gremlin-like path | Starts with traversal forms recognized by the Gremlin mode parser | `g.V().has('name','Ada')` | partial |
| Path | Starts with `PATH` | `PATH FROM a TO b VIA follows` | supported |
| Natural | Free-form text that is not recognized as another mode | `find recent logs about auth failures` | partial |

## Items and RedDB IDs

Query results expose RedDB items with a public envelope. `rid` is the RedDB ID
for the item; it replaces older public aliases such as `_entity_id`,
`red_entity_id`, and `entity_id`.

| Field | Type | Description |
| --- | --- | --- |
| `rid` | `u64` | RedDB ID for the item |
| `collection` | `string` | Source collection |
| `kind` | `string` | Item kind: `row`, `document`, `kv`, `node`, `edge`, or `vector` |
| `tenant` | `string` / `null` | Tenant visible to the statement |
| `created_at` | timestamp/integer millis | Creation timestamp |
| `updated_at` | timestamp/integer millis | Last update timestamp |

The public envelope field names are reserved system fields. Do not define user
columns or top-level document, KV, node, or edge properties named `rid`,
`collection`, `kind`, `tenant`, `created_at`, or `updated_at`.

## SELECT

| Syntax | Example | Status |
| --- | --- | --- |
| `SELECT <projection> FROM <collection>` | `SELECT id, name FROM users` | supported |
| `FROM <collection> SELECT <projection>` | `FROM users SELECT id, name` | supported |
| `WHERE <expr>` | `SELECT * FROM users WHERE age >= $1` | supported |
| `GROUP BY <expr> [HAVING <expr>]` | `SELECT status, COUNT(*) FROM jobs GROUP BY status HAVING COUNT(*) > 1` | supported |
| `ORDER BY <expr> [ASC\|DESC] [NULLS FIRST\|LAST]` | `SELECT * FROM users ORDER BY created_at DESC LIMIT 10` | supported |
| `LIMIT <n> OFFSET <n>` | `SELECT * FROM users LIMIT 25 OFFSET 50` | supported |
| `DISTINCT` | `SELECT DISTINCT status FROM jobs` | supported |
| Projection aliases | `SELECT name AS user_name FROM users` | supported |
| Default expression labels | `SELECT id * 2 FROM users` returns column `id * 2` | supported |
| Table aliases | `SELECT u.name FROM users u` | supported |
| `INNER JOIN` / plain `JOIN` | `SELECT * FROM users JOIN orders ON users.id = orders.user_id` | supported |
| `LEFT [OUTER] JOIN` | `SELECT * FROM users LEFT JOIN orders ON users.id = orders.user_id` | supported |
| `RIGHT [OUTER] JOIN` | `SELECT * FROM users RIGHT JOIN orders ON users.id = orders.user_id` | partial |
| `FULL [OUTER] JOIN` | `SELECT * FROM a FULL JOIN b ON a.id = b.id` | partial |
| `CROSS JOIN` | `SELECT * FROM a CROSS JOIN b` | supported |
| `JOIN GRAPH` | `SELECT h.id FROM hosts h JOIN GRAPH (h)-[:HAS]->(v) ON h.id = v.host_id` | partial |
| Scalar subqueries | `SELECT id FROM users WHERE age > (SELECT AVG(age) FROM users)` | supported |
| `EXISTS` / `NOT EXISTS` | `SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id)` | supported |
| `IN (subquery)` | `SELECT * FROM users WHERE id IN (SELECT user_id FROM orders)` | supported |
| CTEs | `WITH active AS (SELECT * FROM users WHERE active = true) SELECT * FROM active` | supported |
| Time travel `AS OF` | `SELECT * FROM orders AS OF TAG 'v1.0'` | supported |

## DML

| Syntax | Example | Status |
| --- | --- | --- |
| `INSERT INTO <table> (<cols>) VALUES (...)` | `INSERT INTO users (id, name) VALUES (1, 'Ada')` | supported |
| Multi-row `INSERT` | `INSERT INTO users (id) VALUES (1), (2)` | supported |
| Parameterized `INSERT` | `INSERT INTO users (id, name) VALUES ($1, $2)` | supported |
| `INSERT ... RETURNING *` | `INSERT INTO users (name) VALUES ('Ada') RETURNING *` | supported |
| `INSERT ... RETURNING col, ...` | `INSERT INTO users (name) VALUES ('Ada') RETURNING rid, name` | supported |
| `UPDATE ... SET ... WHERE ...` | `UPDATE users SET active = true WHERE rid = $1` | supported |
| Explicit update targets | `UPDATE users ROWS SET active = true WHERE rid = $1` | supported |
| Multi-model update targets | `UPDATE docs DOCUMENTS SET score += 1`; `UPDATE settings KV SET value += 1`; `UPDATE social NODES SET score += 1`; `UPDATE social EDGES SET weight += 0.5` | supported |
| Compound assignment | `UPDATE users SET score += 5, attempts %= 3 WHERE rid = $1` | supported |
| Ordered update batches | `UPDATE users ROWS SET touched = true ORDER BY priority DESC LIMIT 10` | supported; `ORDER BY` requires `LIMIT` |
| `UPDATE ... RETURNING * / cols` | `UPDATE users SET name = 'Ada' WHERE rid = $1 RETURNING rid, name` | supported |
| `DELETE FROM ... WHERE ...` | `DELETE FROM users WHERE rid = $1` | supported |
| `DELETE ... RETURNING * / cols` | `DELETE FROM users WHERE rid = $1 RETURNING *` | supported |
| `RETURNING <expr>` | `INSERT INTO users (id) VALUES (1) RETURNING id + 1` | not yet: `NOT_YET_SUPPORTED` |
| `TRUNCATE <model> <name>` | `TRUNCATE TABLE users` | supported |

## DDL and Catalog

| Syntax | Example | Status |
| --- | --- | --- |
| `CREATE TABLE` | `CREATE TABLE users (id INT PRIMARY KEY, name TEXT DEFAULT = 'unknown')` | supported |
| Table modifiers | `CREATE TABLE events (id INT) APPEND ONLY` | supported |
| Event hooks | `CREATE TABLE users (id INT) WITH EVENTS (INSERT, UPDATE) TO user_events` | supported |
| `ALTER TABLE` | `ALTER TABLE users ADD COLUMN email TEXT` | supported |
| `DROP TABLE [IF EXISTS]` | `DROP TABLE IF EXISTS users` | supported |
| `CREATE [UNIQUE] INDEX ... USING BTREE/HASH` | `CREATE UNIQUE INDEX users_email ON users (email) USING HASH` | supported |
| `DROP INDEX` | `DROP INDEX users_email` | supported |
| `DESCRIBE` / `DESC` | `DESCRIBE users` | supported |
| `SHOW CREATE TABLE` | `SHOW CREATE TABLE users` | supported |
| `SHOW INDEXES` / `SHOW INDICES` | `SHOW INDEXES ON users` | supported |
| `SHOW COLLECTIONS [INCLUDING INTERNAL]` | `SHOW COLLECTIONS WHERE model = 'table'` | supported |
| `SHOW TABLES/QUEUES/VECTORS/DOCUMENTS/TIMESERIES/GRAPHS/KV` | `SHOW TABLES` | supported |
| `SHOW SCHEMA <collection>` | `SHOW SCHEMA users` | supported |
| `SHOW SAMPLE <collection> [LIMIT n]` | `SHOW SAMPLE users LIMIT 5` | supported |
| `SHOW STATS [collection]` | `SHOW STATS users` | supported |
| `CREATE VIEW` / `CREATE MATERIALIZED VIEW` | `CREATE VIEW active AS SELECT * FROM users WHERE active = true` | supported |
| `DROP [MATERIALIZED] VIEW` | `DROP MATERIALIZED VIEW active` | supported |
| `REFRESH MATERIALIZED VIEW` | `REFRESH MATERIALIZED VIEW active` | supported |
| `CREATE COLLECTION` | `CREATE COLLECTION raw` | supported |
| `DROP COLLECTION` | `DROP COLLECTION raw` | supported |
| `CREATE GRAPH/DOCUMENT/VECTOR/KV/CONFIG/VAULT` | `CREATE KV settings` | supported |
| `DROP GRAPH/DOCUMENT/VECTOR/KV/CONFIG/VAULT` | `DROP KV settings` | supported |
| `CREATE SCHEMA` / `DROP SCHEMA` | `CREATE SCHEMA tenant_a` | supported |
| `CREATE SEQUENCE` / `DROP SEQUENCE` | `CREATE SEQUENCE order_ids START WITH 100` | supported |
| `CREATE SERVER` / `CREATE FOREIGN TABLE` | `CREATE SERVER pg FOREIGN DATA WRAPPER postgres OPTIONS (host 'db')` | partial |
| `COPY <table> FROM 'path'` | `COPY users FROM '/tmp/users.csv' WITH (FORMAT csv, HEADER true)` | supported |
| `VACUUM [FULL] [table]` | `VACUUM FULL users` | supported |
| `ANALYZE [table]` | `ANALYZE users` | supported |

## Operators

| Operator | Example | Return type | Status |
| --- | --- | --- | --- |
| Arithmetic `+`, `-`, `*`, `/`, `%` | `score * 2` | numeric | supported |
| Comparison `=`, `!=`, `<>`, `<`, `<=`, `>`, `>=` | `age >= 18` | boolean | supported |
| Boolean `AND`, `OR`, `NOT` | `active = true AND deleted IS NULL` | boolean | supported |
| String concat `||` | `first || ' ' || last` | text | supported |
| `LIKE` | `email LIKE '%@example.com'` | boolean | supported |
| `IN` | `status IN ('open', 'done')` | boolean | supported |
| `BETWEEN` | `age BETWEEN 18 AND 65` | boolean | supported |
| `IS NULL` / `IS NOT NULL` | `deleted_at IS NULL` | boolean | supported |
| `CASE WHEN ... THEN ... ELSE ... END` | `CASE WHEN age >= 18 THEN 'adult' ELSE 'minor' END` | common branch type | supported |

## Builtins and Aggregates

| Function | Signature | Example | Return type | Status |
| --- | --- | --- | --- | --- |
| `NOW()` | `NOW()` | `SELECT NOW()` | timestamp/integer millis | supported |
| `CURRENT_TIMESTAMP` / `CURRENT_TIMESTAMP()` | no args | `SELECT CURRENT_TIMESTAMP` | timestamp/integer millis | supported |
| `CURRENT_DATE` / `CURRENT_TIME` | no args | `SELECT CURRENT_DATE` | date/time text or timestamp-compatible value | supported |
| `CURRENT_TENANT()` | no args | `SELECT CURRENT_TENANT()` | text/null | supported |
| `CURRENT_USER` / `CURRENT_ROLE()` | no args | `SELECT CURRENT_ROLE()` | text/null | supported |
| `UPPER` / `LOWER` | `UPPER(text)` | `SELECT UPPER(name) FROM users` | text | supported |
| `COALESCE` | `COALESCE(a, b, ...)` | `SELECT COALESCE(name, 'unknown') FROM users` | first non-null argument type | supported |
| `COUNT` | `COUNT(*)`, `COUNT(expr)` | `SELECT COUNT(*) FROM users` | integer | supported |
| `SUM` / `AVG` | numeric expression | `SELECT AVG(score) FROM runs` | numeric | supported |
| `MIN` / `MAX` | comparable expression | `SELECT MAX(created_at) FROM users` | input type | supported |
| PostgreSQL math functions | `SQRT`, `POWER`, `EXP`, `LN`, `LOG`, `LOG10`, `SIN`, `COS`, `TAN`, `ASIN`, `ACOS`, `ATAN`, `ATAN2`, `COT`, `DEGREES`, `RADIANS`, `PI` | `SELECT SQRT(score) FROM runs` | float | supported |
| PostgreSQL math aliases | `POW`, `ARCSIN`, `ARCCOS`, `ARCTAN` | `SELECT POW(2, 4)` | float | supported |
| `EMBED` | `EMBED(text)` | `SELECT EMBED(body) FROM docs` | vector | partial: provider configuration required |
| ML/cache scalars | `ML_CLASSIFY(model, features)`, `SEMANTIC_CACHE_GET(key)` | `SELECT ML_CLASSIFY('m', payload) FROM events` | JSON/value | partial |
| Continuous aggregate helpers | `CA_LIST()`, `CA_REFRESH(name)` | `SELECT CA_LIST()` | JSON/value | partial |
| Hypertable helpers | `SHOW_HYPERTABLES()`, `HYPERTABLE_SHOW_CHUNKS(name)` | `SELECT HYPERTABLE_SHOW_CHUNKS('metrics')` | JSON/value | partial |

## GRAPH

| Syntax | Example | Status |
| --- | --- | --- |
| `CREATE GRAPH` / `DROP GRAPH` | `CREATE GRAPH social` | supported |
| `INSERT INTO <graph> NODE (...) VALUES (...)` | `INSERT INTO social NODE (label, node_type) VALUES ('alice', 'User') RETURNING rid` | supported |
| `INSERT INTO <graph> EDGE (...) VALUES (...)` | `INSERT INTO social EDGE (label, from_rid, to_rid) VALUES ('knows', $1, $2)` | supported |
| `MATCH ... RETURN ...` | `MATCH (a)-[:FOLLOWS]->(b) RETURN b` | partial |
| `GRAPH NEIGHBORHOOD` | `GRAPH NEIGHBORHOOD social FROM 'a' DEPTH 2` | supported |
| `GRAPH SHORTEST_PATH` | `GRAPH SHORTEST_PATH social FROM 'a' TO 'b'` | supported |
| `GRAPH TRAVERSE` | `GRAPH TRAVERSE social FROM 'a' DEPTH 3` | supported |
| `GRAPH CENTRALITY` | `GRAPH CENTRALITY social ALGORITHM pagerank` | supported |
| `GRAPH COMMUNITY` | `GRAPH COMMUNITY social ALGORITHM louvain` | supported |
| `GRAPH COMPONENTS` | `GRAPH COMPONENTS social` | supported |
| `GRAPH CYCLES` | `GRAPH CYCLES social` | supported |
| `GRAPH TOPOLOGICAL_SORT` | `GRAPH TOPOLOGICAL_SORT deps` | supported |
| `GRAPH PROPERTIES` | `GRAPH PROPERTIES social` | supported |

## KV, Config, and Vault

| Syntax | Example | Status |
| --- | --- | --- |
| `CREATE KV` / `DROP KV` | `CREATE KV settings` | supported |
| `KV GET/PUT/DELETE` | `KV PUT settings:theme 'dark'` | supported |
| Key `collection:key` rule | `settings:theme` addresses key `theme` in collection `settings` | supported |
| `WATCH <kv>` | `WATCH settings:theme` | supported |
| `SET CONFIG` / `SHOW CONFIG` | `SET CONFIG durability.mode = 'sync'` | supported |
| `LIST CONFIG` / `WATCH CONFIG` / `INVALIDATE CONFIG` | `LIST CONFIG durability` | supported |
| `CREATE VAULT` / `SET SECRET` / `SHOW SECRET` / `DELETE SECRET` | `SET SECRET openai.api_key = 'sk-...'` | supported |
| `UNSEAL VAULT` / `WATCH VAULT` / `LIST VAULT` | `UNSEAL VAULT red.vault` | partial |
| Blob `CACHE` API | HTTP `/cache/*`, SDK `db.cache.*` | supported over HTTP/gRPC clients; unsupported on embedded stdio |

## QUEUE

| Syntax | Example | Status |
| --- | --- | --- |
| `CREATE QUEUE` / `DROP QUEUE` | `CREATE QUEUE jobs PRIORITY` | supported |
| `ALTER QUEUE SET MODE` | `ALTER QUEUE jobs SET MODE fanout` | supported |
| `QUEUE PUSH` / `LPUSH` / `RPUSH` | `QUEUE PUSH jobs '{"task":"ship"}' PRIORITY 10` | supported |
| `QUEUE POP` / `LPOP` / `RPOP` | `QUEUE POP jobs 5` | supported |
| `QUEUE PEEK` | `QUEUE PEEK jobs 10` | supported |
| `QUEUE ACK` / `QUEUE NACK` | `QUEUE ACK jobs 'msg-id'` | supported |
| `QUEUE PURGE` | `QUEUE PURGE jobs` | supported |
| `QUEUE GROUP CREATE/READ` | `QUEUE GROUP CREATE jobs workers` | supported |
| Queue events integration | `CREATE TABLE users (...) WITH EVENTS TO jobs` | supported |

## TIMESERIES

| Syntax | Example | Status |
| --- | --- | --- |
| `CREATE TIMESERIES` / `DROP TIMESERIES` | `CREATE TIMESERIES metrics` | supported |
| `CREATE HYPERTABLE` / `DROP HYPERTABLE` | `CREATE HYPERTABLE metrics TIME COLUMN ts CHUNK INTERVAL 1h` | supported |
| Retention | `CREATE TIMESERIES metrics RETENTION 30d` | supported |
| Downsample / continuous aggregate surface | `CREATE TIMESERIES metrics DOWNSAMPLE 1m` | partial |
| Insert points via SQL table path | `INSERT INTO metrics (ts, value) VALUES (NOW(), 42)` | supported |
| Chunk helpers | `SELECT HYPERTABLE_SHOW_CHUNKS('metrics')` | partial |

## Probabilistic Structures

| Syntax | Example | Status |
| --- | --- | --- |
| `CREATE HLL` / `DROP HLL` | `CREATE HLL visitors` | supported |
| `HLL ADD` / `HLL COUNT` | `HLL ADD visitors 'user-1'` | supported |
| `CREATE SKETCH` / `DROP SKETCH` | `CREATE SKETCH latencies` | supported |
| `SKETCH ADD` / query commands | `SKETCH ADD latencies 12.4` | supported |
| `CREATE FILTER` / `DROP FILTER` | `CREATE FILTER seen` | supported |
| `FILTER ADD/CHECK` | `FILTER CHECK seen 'id-1'` | supported |

## Transactions

| Syntax | Example | Status |
| --- | --- | --- |
| `BEGIN [WORK\|TRANSACTION]` | `BEGIN` | supported |
| `START TRANSACTION` | `START TRANSACTION ISOLATION LEVEL SNAPSHOT` | supported |
| Isolation clauses | `BEGIN ISOLATION LEVEL REPEATABLE READ` | partial: maps to snapshot isolation |
| `SERIALIZABLE` | `BEGIN ISOLATION LEVEL SERIALIZABLE` | not yet |
| `COMMIT` / `ROLLBACK` | `COMMIT` | supported |
| Savepoints | `SAVEPOINT s1`, `ROLLBACK TO s1`, `RELEASE s1` | supported |
| SDK wrapper | `await db.transaction(async (tx) => tx.insert('t', row))` | supported |

## EXPLAIN and Maintenance

| Syntax | Example | Status |
| --- | --- | --- |
| `EXPLAIN ALTER FOR ...` | `EXPLAIN ALTER FOR CREATE TABLE users (id INT)` | supported |
| `EXPLAIN MIGRATION` | `EXPLAIN MIGRATION add_users` | supported |
| `EXPLAIN ASK` | `EXPLAIN ASK 'what changed?'` | supported |
| Generic `EXPLAIN SELECT` | `EXPLAIN SELECT * FROM users` | partial |
| `VACUUM`, `ANALYZE`, `COPY`, `REFRESH MATERIALIZED VIEW` | `ANALYZE users` | supported |

## Notes and Known Edges

| Topic | Current behavior | Status |
| --- | --- | --- |
| Parameter binding | `$N` parameters work across SQL text, PG-wire extended protocol, JS SDK, HTTP, gRPC, and RedWire paths that expose params. | supported |
| JS `bulkInsert` | SDK sends one JSON-RPC `bulk_insert` call containing many rows; binary RedWire has a separate native binary bulk path. | supported |
| Generated column labels | Unaliased expressions use source-like labels such as `id * 2`, `name || '!'`, and `COALESCE(name, 'fb')`. | supported |
| `red.*` catalog | `red.collections`, `red.columns`, `red.indices`, `red.show_indexes`, `red.describe`, and `red.show_create` are the canonical introspection surface. | supported |
| Unsupported syntax | Recognized-but-unimplemented SQL forms should return a clear parser/runtime error rather than silently doing something else. | supported |
