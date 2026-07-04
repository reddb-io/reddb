# Graph Topology

RedDB supports two complementary views of graph topology: **structural topology** (the shape of the graph — connectivity, density, components) and **topological ordering** (a dependency-safe traversal order for DAGs). Both are available through the same SQL and HTTP surface.

## Topological Sort

`GRAPH TOPOLOGICAL_SORT` computes a valid traversal order for a directed acyclic graph (DAG): every node appears before all nodes it points to. This is the basis for build systems, task schedulers, and dependency resolvers.

```sql
GRAPH TOPOLOGICAL_SORT
```

HTTP equivalent:

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/topological-sort \
  -H 'content-type: application/json' \
  -d '{}'
```

The command fails if the graph contains a cycle; use `GRAPH CYCLES` or check `is_acyclic` from `GRAPH PROPERTIES` first.

### Response Shape

```json
{
  "ok": true,
  "order": ["lint", "compile", "test", "package", "publish"],
  "node_count": 5
}
```

`order` lists node labels in dependency-safe sequence: the first entry has no predecessors; the last has no successors.

---

## Use Case: CI/CD Build Pipeline

Model a build pipeline as a DAG where each task depends on earlier ones completing first.

### Build the graph

```sql
-- Build stages as nodes
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('checkout',  'stage', 30);
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('lint',      'stage', 60);
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('compile',   'stage', 120);
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('unit-test', 'stage', 90);
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('int-test',  'stage', 180);
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('package',   'stage', 45);
INSERT INTO pipeline NODE (label, node_type, max_duration_s) VALUES ('publish',   'stage', 20);

-- Dependencies: from = prerequisite, to = dependent
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1024, 1025);  -- checkout → lint
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1024, 1026);  -- checkout → compile
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1026, 1027);  -- compile → unit-test
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1026, 1028);  -- compile → int-test
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1027, 1029);  -- unit-test → package
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1028, 1029);  -- int-test → package
INSERT INTO pipeline EDGE (label, from, to) VALUES ('must_precede', 1029, 1030);  -- package → publish
```

### Confirm the graph is acyclic

```sql
GRAPH PROPERTIES
```

Check that `is_acyclic = true` before running the sort.

### Compute execution order

```sql
GRAPH TOPOLOGICAL_SORT
```

### Detect parallelism opportunities

Stages with no direct dependency between them can run in parallel. Use `GRAPH COMPONENTS` and `GRAPH SHORTEST_PATH` to identify independent chains:

```sql
-- Which stages can be reached from compile?
GRAPH TRAVERSE FROM 'compile' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 5
```

```sql
-- Are lint and compile independent?
GRAPH SHORTEST_PATH FROM 'lint' TO 'compile' ALGORITHM bfs
-- path_found = false → they can run concurrently
```

---

## Use Case: Service Dependency Graph

Model microservice dependencies to answer questions like "which services are affected if `auth` goes down?"

### Build the graph

```sql
INSERT INTO services NODE (label, node_type, team) VALUES ('gateway',   'service', 'platform');
INSERT INTO services NODE (label, node_type, team) VALUES ('auth',      'service', 'security');
INSERT INTO services NODE (label, node_type, team) VALUES ('users',     'service', 'core');
INSERT INTO services NODE (label, node_type, team) VALUES ('billing',   'service', 'finance');
INSERT INTO services NODE (label, node_type, team) VALUES ('payments',  'service', 'finance');
INSERT INTO services NODE (label, node_type, team) VALUES ('mailer',    'service', 'platform');
INSERT INTO services NODE (label, node_type, team) VALUES ('postgres',  'infra',   'platform');

-- Directed dependency edges (from = caller, to = dependency)
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1024, 1025);  -- gateway → auth
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1024, 1026);  -- gateway → users
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1026, 1025);  -- users → auth
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1026, 1030);  -- users → postgres
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1027, 1025);  -- billing → auth
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1027, 1028);  -- billing → payments
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1028, 1030);  -- payments → postgres
INSERT INTO services EDGE (label, from, to) VALUES ('depends_on', 1027, 1029);  -- billing → mailer
```

### Blast radius: services impacted by an auth outage

```sql
-- All services that eventually call auth (incoming traversal from auth)
GRAPH TRAVERSE FROM 'auth' STRATEGY bfs DIRECTION incoming MAX_DEPTH 5
```

### Critical path to a leaf service

```sql
GRAPH SHORTEST_PATH FROM 'gateway' TO 'postgres' ALGORITHM bfs
```

### Safe deployment order (no circular deps)

```sql
GRAPH TOPOLOGICAL_SORT
-- Returns services in safe boot order: infra first, gateway last
```

### Find the most-depended-on service

```sql
GRAPH CENTRALITY ALGORITHM degree ORDER BY centrality_score DESC LIMIT 5
```

---

## Use Case: Network Infrastructure Topology

Model a physical or logical network: routers, switches, hosts, and the links between them.

### Build the topology

```sql
INSERT INTO network NODE (label, node_type, region) VALUES ('core-router',    'router',  'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('edge-router-a',  'router',  'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('edge-router-b',  'router',  'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('switch-rack-1',  'switch',  'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('switch-rack-2',  'switch',  'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('host-web-01',    'host',    'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('host-web-02',    'host',    'us-east-1');
INSERT INTO network NODE (label, node_type, region) VALUES ('host-db-01',     'host',    'us-east-1');

-- Links with bandwidth weights (Gbps)
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1024, 1025, 10.0);  -- core → edge-a
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1024, 1026, 10.0);  -- core → edge-b
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1025, 1027,  1.0);  -- edge-a → switch-1
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1026, 1028,  1.0);  -- edge-b → switch-2
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1027, 1029,  1.0);  -- switch-1 → web-01
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1027, 1030,  1.0);  -- switch-1 → web-02
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1028, 1031,  1.0);  -- switch-2 → db-01
-- Redundant link for fault tolerance
INSERT INTO network EDGE (label, from, to, weight) VALUES ('link', 1025, 1028,  1.0);  -- edge-a → switch-2
```

### Inspect topology shape

```sql
GRAPH PROPERTIES
```

Key fields:
- `is_connected` — is the network fully reachable?
- `is_strongly_connected` — can every node reach every other (bidirectional)?
- `density` — how saturated is the link matrix?

### Find the best path from web to db

```sql
-- Fewest hops
GRAPH SHORTEST_PATH FROM 'host-web-01' TO 'host-db-01' ALGORITHM bfs

-- Highest bandwidth path (invert weights or use Dijkstra on cost)
GRAPH SHORTEST_PATH FROM 'host-web-01' TO 'host-db-01' ALGORITHM dijkstra
```

### Identify single points of failure

```sql
-- High betweenness = bottleneck node
GRAPH CENTRALITY ALGORITHM betweenness ORDER BY centrality_score DESC LIMIT 5
```

### Explore the local neighborhood of a switch

```sql
GRAPH NEIGHBORHOOD 'switch-rack-1' DIRECTION both DEPTH 1
```

### Find all hosts reachable through a specific router

```sql
GRAPH TRAVERSE FROM 'edge-router-a' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 3
```

### Verify fault tolerance: can web-01 still reach db-01 if edge-a fails?

```bash
# Remove the edge-a links temporarily, then re-check connectivity
curl -X POST http://127.0.0.1:5000/graph/analytics/properties \
  -H 'content-type: application/json' \
  -d '{}'
```

---

## Use Case: Package Dependency Resolution

Resolve the install order for a package manager. Each package declares its dependencies; topological sort gives a safe install sequence.

```sql
INSERT INTO packages NODE (label, node_type, version) VALUES ('app',     'package', '2.1.0');
INSERT INTO packages NODE (label, node_type, version) VALUES ('web-fw',  'package', '5.0.0');
INSERT INTO packages NODE (label, node_type, version) VALUES ('router',  'package', '3.2.1');
INSERT INTO packages NODE (label, node_type, version) VALUES ('http',    'package', '1.4.0');
INSERT INTO packages NODE (label, node_type, version) VALUES ('crypto',  'package', '0.9.2');
INSERT INTO packages NODE (label, node_type, version) VALUES ('stdlib',  'package', '2.0.0');

-- app → web-fw → router → http → stdlib
--                        → crypto → stdlib
INSERT INTO packages EDGE (label, from, to) VALUES ('requires', 1024, 1025);  -- app → web-fw
INSERT INTO packages EDGE (label, from, to) VALUES ('requires', 1025, 1026);  -- web-fw → router
INSERT INTO packages EDGE (label, from, to) VALUES ('requires', 1026, 1027);  -- router → http
INSERT INTO packages EDGE (label, from, to) VALUES ('requires', 1027, 1029);  -- http → stdlib
INSERT INTO packages EDGE (label, from, to) VALUES ('requires', 1027, 1028);  -- http → crypto
INSERT INTO packages EDGE (label, from, to) VALUES ('requires', 1028, 1029);  -- crypto → stdlib
```

```sql
-- Detect circular deps before resolving
GRAPH CYCLES MAX_LENGTH 10

-- Safe install order
GRAPH TOPOLOGICAL_SORT
-- Expected: stdlib, crypto, http, router, web-fw, app
```

---

## Checking Topology Before Sort

`GRAPH TOPOLOGICAL_SORT` fails on cyclic graphs. The standard pre-flight sequence:

```sql
-- 1. Confirm no cycles
GRAPH CYCLES MAX_LENGTH 20

-- 2. Confirm the graph is a DAG
GRAPH PROPERTIES
-- Inspect: is_acyclic = true

-- 3. Run the sort
GRAPH TOPOLOGICAL_SORT
```

HTTP form of the full sequence:

```bash
# Step 1: cycle check
curl -X POST http://127.0.0.1:5000/graph/analytics/cycles \
  -H 'content-type: application/json' \
  -d '{"max_length": 20}'

# Step 2: properties (is_acyclic)
curl -X POST http://127.0.0.1:5000/graph/analytics/properties \
  -H 'content-type: application/json' \
  -d '{}'

# Step 3: sort
curl -X POST http://127.0.0.1:5000/graph/analytics/topological-sort \
  -H 'content-type: application/json' \
  -d '{}'
```

---

## Related Commands

- [Cycle Detection](/graph/cycles.md) — detect circular dependencies before sorting
- [Graph Properties](/graph/properties.md) — `is_acyclic`, `is_tree`, connectivity metrics
- [Pathfinding Algorithms](/graph/pathfinding.md) — shortest paths through a topology
- [Graph Commands](/query/graph-commands.md) — full SQL command reference
