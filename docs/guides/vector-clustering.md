# Vector Clustering

RedDB provides standalone vector clustering operations that partition vectors in a collection into groups based on similarity. Two algorithms are available: K-Means for a known number of clusters and DBSCAN for automatic density-based discovery.

---

## Algorithms

### K-Means

Partitions N vectors into K clusters by iteratively assigning each vector to its nearest centroid and recomputing centroids as cluster means.

**How it works:**
1. **Initialization (K-Means++)**: Selects K initial centroids spread far apart using weighted probability based on distance to existing centroids. This avoids poor starting positions that can trap the algorithm in local minima.
2. **Assignment**: Each vector is assigned to the nearest centroid (L2 distance, SIMD-accelerated).
3. **Update**: Each centroid is recomputed as the mean of all vectors assigned to it.
4. **Convergence**: Steps 2-3 repeat until centroids stop moving (shift < threshold) or max iterations reached.

**Parameters:**
| Parameter | Default | Description |
|:----------|:--------|:------------|
| `k` | required | Number of clusters |
| `max_iterations` | 100 | Maximum iteration count |
| `convergence_threshold` | 0.001 | Stop when max centroid shift falls below this |

**Performance**: The assignment step (step 2) is parallelized using `std::thread::scope` for datasets larger than 1,000 vectors. Each thread processes a chunk of vectors independently, with automatic CPU core detection to skip parallelism on single-core machines.

**Complexity**: O(N * K * D * I) where N = vectors, K = clusters, D = dimensions, I = iterations.

### DBSCAN

Density-Based Spatial Clustering of Applications with Noise. Discovers clusters of arbitrary shape without requiring a pre-specified K. Points that don't belong to any dense region are labeled as noise.

**How it works:**
1. For each unvisited point, find all neighbors within distance `eps`.
2. If a point has at least `min_points` neighbors, it starts a new cluster.
3. Expand the cluster by recursively adding density-reachable points.
4. Points that are not reachable from any dense region are labeled noise (-1).

**Parameters:**
| Parameter | Default | Description |
|:----------|:--------|:------------|
| `eps` | 0.5 | Maximum L2 distance between two points to be neighbors |
| `min_points` | 3 | Minimum neighbors required to form a dense region |

**When to use DBSCAN over K-Means:**
- You don't know how many clusters exist
- Clusters have irregular shapes (non-spherical)
- You need noise detection (outlier identification)
- Data has varying cluster densities

**Complexity**: O(N²) in the worst case (distance matrix). For large datasets, consider using K-Means or pre-filtering with a bounding box.

---

## HTTP API

### POST /vectors/cluster

Cluster all vectors in a collection.

#### K-Means example

```bash
curl -X POST localhost:8080/vectors/cluster -d '{
  "collection": "products",
  "field": "embedding",
  "algorithm": "kmeans",
  "k": 5,
  "max_iterations": 100
}'
```

#### DBSCAN example

```bash
curl -X POST localhost:8080/vectors/cluster -d '{
  "collection": "products",
  "field": "embedding",
  "algorithm": "dbscan",
  "eps": 0.5,
  "min_points": 3
}'
```

#### Request fields

| Field | Type | Required | Description |
|:------|:-----|:---------|:------------|
| `collection` | string | yes | Collection containing the vectors |
| `field` | string | no | Field name holding the vector (default: `"embedding"`) |
| `algorithm` | string | no | `"kmeans"` (default) or `"dbscan"` |
| `k` | number | kmeans | Number of clusters |
| `max_iterations` | number | no | Max iterations for K-Means (default: 100) |
| `eps` | number | dbscan | Max neighbor distance (default: 0.5) |
| `min_points` | number | dbscan | Min neighbors for dense region (default: 3) |

#### Response

```json
{
  "ok": true,
  "algorithm": "kmeans",
  "k": 5,
  "iterations": 12,
  "converged": true,
  "cluster_sizes": [120, 85, 200, 45, 50],
  "total_vectors": 500,
  "assignments": [
    {"entity_id": 1, "cluster_id": 0},
    {"entity_id": 2, "cluster_id": 2},
    {"entity_id": 3, "cluster_id": 0},
    {"entity_id": 4, "cluster_id": 4}
  ]
}
```

For DBSCAN, `cluster_id: -1` indicates noise (outlier).

---

## Vector Sources

The clustering endpoint looks for vectors in two places, in order:

1. **Embedding slots**: If the entity has embeddings (via `INSERT ... WITH AUTO EMBED` or manual embedding), the first embedding vector is used.
2. **Row field**: If no embedding exists, the specified `field` is checked for a `Value::Vector`.

This means you can cluster both explicitly stored vectors and auto-generated embeddings.

---

## Choosing Parameters

### K-Means: choosing K

If you don't know the right K, try the **elbow method**: run K-Means for K = 2, 3, 4, ... and plot the total within-cluster variance. The "elbow" point where variance stops dropping sharply is a good K.

```bash
# Try different K values
for k in 2 3 4 5 6 7 8 9 10; do
  echo "K=$k"
  curl -s localhost:8080/vectors/cluster \
    -d "{\"collection\":\"products\",\"algorithm\":\"kmeans\",\"k\":$k}" \
    | jq '.cluster_sizes'
done
```

### DBSCAN: choosing eps

The `eps` parameter depends on your data scale. For normalized embeddings (L2 norm = 1), typical values are:

| eps | Effect |
|:----|:-------|
| 0.1 - 0.3 | Tight clusters, many noise points |
| 0.3 - 0.7 | Moderate density, balanced |
| 0.7 - 1.5 | Loose clusters, few noise points |
| > 1.5 | Most points in one cluster |

For unnormalized vectors, eps should reflect the typical intra-cluster distance in your data.

---

## Example: Product Segmentation

```bash
# 1. Insert products with embeddings
curl localhost:8080/query -d '{
  "query": "INSERT INTO products (name, category, price) VALUES (\"Widget A\", \"tools\", 29.99) WITH AUTO EMBED"
}'
# ... (repeat for many products)

# 2. Cluster into segments
curl -X POST localhost:8080/vectors/cluster -d '{
  "collection": "products",
  "field": "embedding",
  "algorithm": "kmeans",
  "k": 4
}'

# 3. Use assignments to tag products
# Each entity_id maps to a cluster_id (0, 1, 2, 3)
# Update products with their cluster assignment
```

## Example: Outlier Detection

```bash
# Use DBSCAN to find anomalous network traffic patterns
curl -X POST localhost:8080/vectors/cluster -d '{
  "collection": "network_flows",
  "field": "feature_vector",
  "algorithm": "dbscan",
  "eps": 1.2,
  "min_points": 5
}'

# Entities with cluster_id: -1 are anomalies
```

---

## Performance

| Dataset size | K-Means (K=10) | DBSCAN (eps=0.5) |
|:-------------|:---------------|:-----------------|
| 1,000 vectors | ~5 ms | ~10 ms |
| 10,000 vectors | ~50 ms (parallel) | ~500 ms |
| 100,000 vectors | ~500 ms (parallel) | ~50 s |

K-Means scales linearly with N. DBSCAN scales quadratically (N²) due to pairwise distance computation. For datasets larger than ~50K vectors, K-Means is recommended.

Both algorithms use SIMD-accelerated L2 distance computation (SSE/AVX auto-detected at runtime).
