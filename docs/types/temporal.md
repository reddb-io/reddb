# Temporal Types

Types for dates, times, timestamps, and durations.

## Timestamp

Unix timestamp in seconds since epoch (i64, 8 bytes).

```sql
CREATE TABLE events (occurred_at Timestamp NOT NULL)
```

```rust
Value::Timestamp(1705312200)  // 2024-01-15T10:30:00Z
```

## TimestampMs

Timestamp with millisecond precision (i64, 8 bytes).

```rust
Value::TimestampMs(1705312200000)  // 2024-01-15T10:30:00.000Z
```

## Date

Date only, stored as i32 days since Unix epoch (4 bytes). No time component.

```sql
CREATE TABLE logs (date Date NOT NULL)
```

```rust
Value::Date(19738)  // 2024-01-15
```

## Time

Time only, stored as u32 milliseconds since midnight (4 bytes). No date component.

```sql
CREATE TABLE schedules (start_time Time NOT NULL)
```

```rust
Value::Time(37800000)  // 10:30:00.000
```

## Duration

Duration in milliseconds (i64, 8 bytes).

```sql
CREATE TABLE performance (response_time Duration)
```

```rust
Value::Duration(1500)  // 1.5 seconds
```

## Example: Event Log

```sql
CREATE TABLE audit_log (
  event_date Date NOT NULL,
  event_time Time NOT NULL,
  created_at TimestampMs NOT NULL,
  duration Duration
)
```
