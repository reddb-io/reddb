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
    byte[] body = conn.query("SELECT * FROM users WHERE name = 'alice'");
    System.out.println(new String(body, java.nio.charset.StandardCharsets.UTF_8));
}
```

Use `Options.builder().username(...).password(...)` to enable
SCRAM-SHA-256 over RedWire, or `.token(...)` for bearer auth.

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
`RED_SMOKE=1`:

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
