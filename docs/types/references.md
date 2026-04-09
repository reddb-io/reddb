# Reference Types

Reference types create cross-entity links between different data models. They enable unified queries across tables, graphs, vectors, and documents.

## NodeRef

Reference to a graph node by entity ID.

```rust
Value::NodeRef(42)  // References node with entity_id=42
```

## EdgeRef

Reference to a graph edge by entity ID.

```rust
Value::EdgeRef(101)  // References edge with entity_id=101
```

## VectorRef

Reference to a vector embedding by entity ID.

```rust
Value::VectorRef(200)  // References vector with entity_id=200
```

## RowRef

Reference to a table row (table_id, row_id). Stored as 16 bytes.

```rust
Value::RowRef("users", 1)  // References row 1 in the users table
```

## KeyRef

Reference to a KV pair (collection name + key string).

```rust
Value::KeyRef("config", "max_retries")
```

## DocRef

Reference to a document (collection name + entity_id).

```rust
Value::DocRef("events", 42)
```

## TableRef

Reference to a table/collection by name.

```rust
Value::TableRef("users")
```

## PageRef

Reference to a physical storage page. Used internally by the storage engine.

## Example: Cross-Model Linking

Store a row that references a graph node and a vector:

```sql
CREATE TABLE host_index (
  ip Text NOT NULL,
  graph_node NodeRef,
  embedding VectorRef,
  document DocRef
)
```

```rust
let host_id = db.row("host_index", vec![
    ("ip", Value::Text("10.0.0.1".into())),
    ("graph_node", Value::NodeRef(node_id)),
    ("embedding", Value::VectorRef(vector_id)),
    ("document", Value::DocRef("logs", doc_id)),
]).save()?;
```

This pattern lets you maintain explicit cross-references between your different data models while keeping them queryable through the universal query engine.
