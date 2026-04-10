# Probabilistic Data Structures

RedDB includes three built-in probabilistic data structures for real-time analytics at scale. These are first-class SQL commands -- no extensions or plugins required.

## Overview

| Structure | Purpose | Memory | Error | Deletion? |
|:----------|:--------|:-------|:------|:----------|
| **HyperLogLog** | Count distinct elements | ~16 KB | ~0.81% | No |
| **Count-Min Sketch** | Estimate frequency | ~40 KB | Overestimates | No |
| **Cuckoo Filter** | Membership testing | ~100 KB per 100K items | < 3% FP | Yes |

## HyperLogLog (HLL)

Estimates the number of **distinct elements** in a set. Uses ~16 KB of memory regardless of how many elements you add.

**Use cases:** Unique visitor counting, distinct user estimation, cardinality queries.

### Commands

```sql
-- Create
CREATE HLL visitors

-- Add elements (duplicates are handled automatically)
HLL ADD visitors 'user_alice' 'user_bob' 'user_alice'

-- Get approximate count of distinct elements
HLL COUNT visitors
-- Result: {"count": 2}

-- Count across multiple HLLs (union cardinality)
HLL COUNT visitors_us visitors_eu

-- Merge into a new HLL
HLL MERGE global_visitors visitors_us visitors_eu

-- Get info (count, memory usage)
HLL INFO visitors

-- Drop
DROP HLL visitors
```

### Accuracy

With 16,384 registers, HyperLogLog provides:

| Elements | Typical Error |
|:---------|:-------------|
| 1,000 | < 3% |
| 100,000 | < 1% |
| 10,000,000 | < 1% |

Memory usage is constant: **~16 KB** per HLL, regardless of cardinality.

## Count-Min Sketch (CMS)

Estimates the **frequency** of elements in a stream. Always overestimates, never underestimates. Useful for top-K queries and anomaly detection.

**Use cases:** Click counting, hot-key detection, rate limiting, frequency analysis.

### Commands

```sql
-- Create with default dimensions (width=1000, depth=5)
CREATE SKETCH click_counter

-- Create with custom dimensions (more width = more accurate)
CREATE SKETCH click_counter WIDTH 2000 DEPTH 7

-- Increment count (default increment = 1)
SKETCH ADD click_counter 'button_signup'

-- Increment by N
SKETCH ADD click_counter 'button_signup' 5

-- Estimate frequency
SKETCH COUNT click_counter 'button_signup'
-- Result: {"estimate": 6}

-- Merge sketches
SKETCH MERGE combined sketch_region1 sketch_region2

-- Get info
SKETCH INFO click_counter
-- Result: {"name": "click_counter", "width": 2000, "depth": 7, "total": 42, "memory_bytes": 80120}

-- Drop
DROP SKETCH click_counter
```

### Accuracy

| Width | Depth | Memory | Error bound |
|:------|:------|:-------|:------------|
| 1,000 | 5 | ~40 KB | total/1000 per query |
| 2,000 | 7 | ~112 KB | total/2000 per query |
| 10,000 | 7 | ~560 KB | total/10000 per query |

Higher width = smaller error. Higher depth = higher confidence.

## Cuckoo Filter

Tests whether an element is **probably in a set** or **definitely not in a set**. Unlike Bloom filters, supports **deletion**. Unlike HyperLogLog, answers membership questions (not cardinality).

**Use cases:** Session tracking, duplicate detection, allow/deny lists, cache filtering.

### Commands

```sql
-- Create with default capacity (100,000 elements)
CREATE FILTER active_sessions

-- Create with custom capacity
CREATE FILTER active_sessions CAPACITY 500000

-- Add an element
FILTER ADD active_sessions 'session_abc123'

-- Check membership
FILTER CHECK active_sessions 'session_abc123'
-- Result: {"exists": true}

FILTER CHECK active_sessions 'nonexistent'
-- Result: {"exists": false}

-- Delete an element (not possible with Bloom filters)
FILTER DELETE active_sessions 'session_abc123'

-- Count stored elements
FILTER COUNT active_sessions
-- Result: {"count": 0}

-- Get info
FILTER INFO active_sessions
-- Result: {"name": "active_sessions", "count": 0, "load_factor": 0.0, "memory_bytes": 131120}

-- Drop
DROP FILTER active_sessions
```

### False Positive Rate

With 1-byte fingerprints and bucket size of 4:

| Load Factor | Approx FP Rate |
|:------------|:---------------|
| 50% | < 1% |
| 75% | < 2% |
| 95% | < 3% |

## When to Use Which

| Question | Use |
|:---------|:----|
| "How many unique X?" | **HyperLogLog** |
| "How often does X appear?" | **Count-Min Sketch** |
| "Is X in the set?" (with deletion) | **Cuckoo Filter** |
| "Is X in the set?" (no deletion needed) | Bloom Filter (internal) |

## Persistence

Probabilistic structures are persisted during checkpoint/flush cycles and restored on database restart. They live in memory for fast access and are serialized to disk for durability.

## See Also

- [Search Commands](/query/search-commands.md) -- Vector and text search
- [Tables](/data-models/tables.md) -- When you need exact counts, use `SELECT COUNT(DISTINCT ...)`
