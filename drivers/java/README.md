# reddb-jvm

Java / JVM driver for [RedDB](../../). Speaks RedWire (`red://`,
`reds://`) and HTTP (`http://`, `https://`) on JDK 17+.

## Install

```kotlin
dependencies {
    implementation("dev.reddb:reddb-jvm:0.1.0")
}
```

## Quick start

```java
import dev.reddb.Conn;
import dev.reddb.Options;
import dev.reddb.Reddb;

try (Conn conn = Reddb.connect("red://localhost:5050")) {
    conn.ping();
    conn.insert("users", java.util.Map.of("name", "alice", "age", 30));
    byte[] body = conn.query("SELECT * FROM users WHERE age = $1 AND name = $2", 30, "alice");
    System.out.println(new String(body, java.nio.charset.StandardCharsets.UTF_8));
}
```

Use `Options.builder().username(...).password(...)` to enable
SCRAM-SHA-256 over RedWire, or `.token(...)` for bearer auth.

## Parameterized queries

`query(String sql, Object... params)` binds positional `$N` placeholders.
The original `query(String sql)` call is unchanged, and empty params keep
using the legacy RedWire `Query` frame.

```java
byte[] rows = conn.query(
    "VECTOR SEARCH embeddings SIMILAR TO $1 LIMIT $2",
    new float[]{0.1f, 0.2f, 0.3f},
    10
);

byte[] prepared = conn.prepare("SELECT * FROM users WHERE id = $1")
    .bind(42)
    .query();
```

Native mapping:

| Java type | RedDB value |
|-----------|-------------|
| `null` | `Null` |
| `Boolean` | `Bool` |
| `Byte`, `Short`, `Integer`, `Long` | `Int` |
| `Float`, `Double` | `Float` |
| `String` | `Text` |
| `byte[]` | `Bytes` |
| `float[]` | `Vector` |
| `Map`, `List`, Jackson `JsonNode` | `Json` |
| `Instant` | `Timestamp` |
| `UUID` | `Uuid` |

RedWire parameterized queries require the server to advertise
`FEATURE_PARAMS`; otherwise the driver throws
`RedDBException.ParamsUnsupported`. HTTP sends the same typed values as
the `/query` JSON `params` array.

## Rich helpers (SDK Helper Spec v1.0)

Canonical spec: [`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md)
(version exposed at runtime via `Helpers.HELPER_SPEC_VERSION` = `"1.0"`).

`dev.reddb.helpers.Helpers` wraps a `Conn` with four first-class
namespaces — `documents()`, `kv()`, `queues()` (alias `queue()`), and
`tx()` — mirroring the Go / Dart / Python helpers. Helpers are pure SQL
builders + envelope normalisation; the same wire request works across
RedWire and HTTP.

```java
import dev.reddb.helpers.Helpers;
import dev.reddb.helpers.Envelopes;

var helpers = Helpers.of(conn);

// Documents
Envelopes.InsertResult ins = helpers.documents()
    .insert("people", java.util.Map.of("name", "alice"));
var row = helpers.documents().get("people", ins.rid());

// KV (default collection: kv_default)
helpers.kv().set("characters:hansel", "ok");
Object v = helpers.kv().get("characters:hansel");
var list = helpers.kv().list(new dev.reddb.helpers.KvClient.KvListOptions().prefix("characters:"));

// Queue
var push = helpers.queue().push("jobs", java.util.Map.of("id", 1),
    new dev.reddb.helpers.QueueClient.PushOptions().priority(5));
long len = helpers.queue().len("jobs");
var jobs = helpers.queue().pop("jobs", 10);
```

Typed errors (mirror Go/Python): `HelperException.InvalidArgument`,
`HelperException.NotFound`, `HelperException.InvalidResponse`. These
map onto the spec error codes `INVALID_ARGUMENT`, `NOT_FOUND`,
`INVALID_RESPONSE`. Server-side errors (`CONFLICT`,
`UNAUTHENTICATED`, `PERMISSION_DENIED`, `UNAVAILABLE`,
`FEATURE_DISABLED`, `INTERNAL`) propagate as `RedDBException` with the
server code preserved verbatim.

### Envelopes

| Envelope            | Required fields                                            |
|---------------------|-------------------------------------------------------------|
| `InsertResult`      | `affected`, `rid`, `item`                                   |
| `DeleteResult`      | `affected`, `deleted` (bool: `affected > 0`)                |
| `ExistsResult`      | `exists`                                                    |
| `ListResult`        | `items`, optional `nextCursor`                              |
| `QueuePushResult`   | `affected`, `rid`                                           |

### Transactions

Both forms are supported (imperative + callback). Nested `tx.run`
rejects with `INVALID_ARGUMENT` — callers needing savepoints issue
`SAVEPOINT`/`RELEASE` directly via `conn.query`.

```java
// Imperative
helpers.tx().begin();
helpers.documents().insert("events", java.util.Map.of("k", "v"));
helpers.tx().commit();

// Callback (commit on return, rollback on throw)
helpers.tx().run(t -> {
    helpers.documents().insert("events", java.util.Map.of("k", "v"));
});
```

### Helper availability matrix (spec §12 case IDs)

| Case ID                              | Status |
|--------------------------------------|--------|
| `generic.query.no_params`            | ok |
| `generic.query_with.params`          | ok |
| `generic.insert.rid`                 | ok |
| `generic.bulk_insert.rids`           | ok (via repeated `documents.insert` until single-shot bulk lifts) |
| `generic.delete`                     | ok |
| `documents.crud_nested_patch`        | ok |
| `documents.delete_missing_no_error`  | ok |
| `documents.patch_empty_rejects`      | ok |
| `kv.exact_key_round_trip`            | ok |
| `kv.missing_get_returns_none`        | ok |
| `kv.delete_returns_envelope`         | ok |
| `queues.fifo_peek_pop_len`           | ok |
| `queues.empty_pop_returns_empty`     | ok |
| `queues.purge_resets_len`            | ok |
| `tx.commit_persists`                 | ok |
| `tx.rollback_discards`               | ok |
| `errors.not_found.document_get`      | ok |
| `wire.probabilistic.hll_round_trip`  | ok (provisional) |
| `wire.vectors.sql_round_trip`        | provisional — reachable via `conn.query` |
| `wire.graph.sql_round_trip`          | provisional — reachable via `conn.query` |
| `wire.timeseries.sql_round_trip`     | provisional — reachable via `conn.query` |

### Conformance harness

`drivers/java/src/test/java/dev/reddb/helpers/ConformanceTest.java`
spawns one `red server` process per JUnit run and exercises the case
IDs above. Gated on `RED_SMOKE=1`:

```
RED_SMOKE=1 RED_BIN=/path/to/red ./gradlew test --tests \
    "dev.reddb.helpers.ConformanceTest"
```

### Out-of-scope for v1.0

Per spec §8–§11, these helper namespaces are **provisional** — reach
the same wire surface via `conn.query` / `conn.query(sql, params)`
until v1.1 lifts them into helpers: `vectors.*`, `graph.*`,
`timeseries.*`, `probabilistic.*`. KV TTL helpers, queue
priority / consumer-group sugar, JSON Patch (RFC 6902), deep-merge
`documents.patch`, and isolation-level arguments on `tx.begin` are all
deferred — see the spec for one-line rationale per item.

## URL grammar

| Scheme    | Transport         | Default port | TLS |
|-----------|-------------------|--------------|-----|
| `red://`  | RedWire (TCP)     | 5050         | no  |
| `reds://` | RedWire (TLS)     | 5050         | yes |
| `http://` | HTTP REST         | 5050         | no  |
| `https://`| HTTPS REST        | 5050         | yes |

`red://`, `red://memory`, `red:///path/file.rdb` are reserved for the
embedded engine — they currently throw
`UnsupportedOperationException` until the JNI binding ships.

## Auth

* No credentials → anonymous (server must allow it).
* `Options.token("...")` → bearer / API key.
* `Options.username("...").password("...")` → SCRAM-SHA-256 (RFC 5802).
* HTTPS path: same options trigger `POST /auth/login` first.

## Build

```
./gradlew check
./gradlew test
```

End-to-end smoke against a real engine is gated on
`RED_SMOKE=1`. Set `RED_BIN=/path/to/red` to reuse an existing binary:

```
RED_SMOKE=1 ./gradlew test --tests dev.reddb.SmokeTest
```

## Production deploy

When you're ready to point this driver at a production RedDB cluster:

- **Run RedDB with the encrypted vault** so auth state and
  `red.secret.*` values are protected at rest. See
  [`docs/security/vault.md`](../../docs/security/vault.md).
- **Use Docker secrets or your cloud secret manager** to inject the
  certificate — never bake it into an image. See
  [`docs/getting-started/docker.md`](../../docs/getting-started/docker.md).
- **Track every secret** the driver consumes (bearer tokens, mTLS
  cert + key, OAuth JWTs) in
  [`docs/operations/secrets.md`](../../docs/operations/secrets.md).
- **Use `reds://` (TLS)** or `red://...?tls=true` for any traffic
  crossing the network — never plain `red://` outside localhost.
- **TLS posture, mTLS, OAuth/JWT and reverse-proxy patterns** are
  covered in [`docs/security/transport-tls.md`](../../docs/security/transport-tls.md).
- See [Policies](../../docs/security/policies.md) for IAM-style authorization.
