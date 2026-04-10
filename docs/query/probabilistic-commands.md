# Probabilistic Commands

SQL commands for HyperLogLog, Count-Min Sketch, and Cuckoo Filter. These are first-class data structures managed through the query language.

## HyperLogLog (HLL)

Approximate distinct counting with ~0.81% standard error and ~16 KB memory.

```sql
CREATE HLL <name> [IF NOT EXISTS]
HLL ADD <name> '<element1>' '<element2>' ...
HLL COUNT <name> [<name2> ...]
HLL MERGE <dest> <source1> <source2> ...
HLL INFO <name>
DROP HLL <name> [IF EXISTS]
```

### Example: Unique Visitors

```sql
CREATE HLL daily_visitors

-- Track page views (duplicates handled automatically)
HLL ADD daily_visitors 'user_123' 'user_456' 'user_123'

-- Approximate count of distinct visitors
HLL COUNT daily_visitors
-- {"count": 2}

-- Merge regional counters
HLL MERGE all_visitors us_visitors eu_visitors

-- Check memory usage
HLL INFO daily_visitors
-- {"name": "daily_visitors", "count": 2, "memory_bytes": 16408}
```

## Count-Min Sketch (SKETCH)

Frequency estimation. Always overestimates, never underestimates.

```sql
CREATE SKETCH <name> [WIDTH <w>] [DEPTH <d>] [IF NOT EXISTS]
SKETCH ADD <name> '<element>' [<count>]
SKETCH COUNT <name> '<element>'
SKETCH MERGE <dest> <source1> <source2> ...
SKETCH INFO <name>
DROP SKETCH <name> [IF EXISTS]
```

### Example: Click Tracking

```sql
CREATE SKETCH clicks WIDTH 2000 DEPTH 7

SKETCH ADD clicks 'btn_signup'
SKETCH ADD clicks 'btn_signup' 5
SKETCH ADD clicks 'btn_login' 3

SKETCH COUNT clicks 'btn_signup'
-- {"estimate": 6}

SKETCH INFO clicks
-- {"name": "clicks", "width": 2000, "depth": 7, "total": 9, "memory_bytes": 112120}
```

## Cuckoo Filter (FILTER)

Membership testing with deletion support.

```sql
CREATE FILTER <name> [CAPACITY <n>] [IF NOT EXISTS]
FILTER ADD <name> '<element>'
FILTER CHECK <name> '<element>'
FILTER DELETE <name> '<element>'
FILTER COUNT <name>
FILTER INFO <name>
DROP FILTER <name> [IF EXISTS]
```

### Example: Session Tracking

```sql
CREATE FILTER sessions CAPACITY 500000

FILTER ADD sessions 'sess_abc123'
FILTER CHECK sessions 'sess_abc123'
-- {"exists": true}

FILTER DELETE sessions 'sess_abc123'
FILTER CHECK sessions 'sess_abc123'
-- {"exists": false}
```

## See Also

- [Probabilistic Structures](/data-models/probabilistic.md) -- Detailed guide with accuracy tables
- [Search Commands](/query/search-commands.md) -- Vector and text search
