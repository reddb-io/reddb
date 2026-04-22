# B-Tree Index

RedDB uses B-Trees as the primary index structure for all entity lookups and range scans.

## Overview

The B-Tree implementation supports:

- Point lookups by entity ID
- Range scans with start/end bounds
- Ordered iteration
- Efficient insert and delete
- Page-level persistence

## Structure

```mermaid
flowchart TB
    R[Root Node<br/>keys: 50, 100]
    L1[Internal<br/>keys: 25]
    L2[Internal<br/>keys: 75]
    L3[Internal<br/>keys: 125]
    D1[Leaf: 1-25]
    D2[Leaf: 26-50]
    D3[Leaf: 51-75]
    D4[Leaf: 76-100]
    D5[Leaf: 101-125]
    D6[Leaf: 126-150]

    R --> L1
    R --> L2
    R --> L3
    L1 --> D1
    L1 --> D2
    L2 --> D3
    L2 --> D4
    L3 --> D5
    L3 --> D6
```

## Operations

| Operation | Complexity | Description |
|:----------|:-----------|:------------|
| Point lookup | O(log n) | Find entity by ID |
| Range scan | O(log n + k) | Scan k entities in a range |
| Insert | O(log n) | Insert with page splits |
| Delete | O(log n) | Delete with page merges |
| Ordered iteration | O(n) | Full scan in key order |

## Bulk Insert Fast Path

When the incoming keys are already sorted (the wire bulk protocol
and time-series chunk writer both guarantee this), the B-tree
streams them into leaves without re-descending from the root per
key. When a leaf fills up, the cursor hops to the right sibling via
the sibling pointer and keeps filling — root-to-leaf traversal
only happens once per split, not once per key.

## Page Splits

When a leaf node is full, it splits into two nodes:

1. Allocate a new page
2. Move the upper half of entries to the new page
3. Insert a separator key into the parent
4. If the parent is full, split recursively

## Index Types

RedDB uses B-Trees for:

- **Primary index**: Entity ID lookups per collection
- **Secondary indexes**: Column-based indexes for filtered queries
- **Graph indexes**: Node label and edge label lookups

## Configuration

B-Tree parameters are tuned for the page size:

| Parameter | Value | Description |
|:----------|:------|:------------|
| Branching factor | ~128 | Keys per internal node |
| Leaf capacity | ~64 | Entries per leaf node |
| Page size | 4096 bytes | Default page size |
