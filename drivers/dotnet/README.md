# Reddb (.NET driver)

Official .NET driver for [RedDB](https://github.com/reddb/reddb). Speaks
the **RedWire** binary protocol (TCP + TLS) and the JSON HTTP API.

* Target framework: `net8.0`
* NuGet package id: `Reddb`
* License: MIT

## Install

```bash
dotnet add package Reddb
```

## Quick start

```csharp
using Reddb;

await using IConn conn = await Reddb.ConnectAsync("red://localhost:5050");

await conn.InsertAsync("users", new { name = "alice", age = 30 });

ReadOnlyMemory<byte> rows = await conn.QueryAsync("SELECT * FROM users");
string json = System.Text.Encoding.UTF8.GetString(rows.Span);

ReadOnlyMemory<byte> filtered = await conn.QueryAsync(
    "SELECT * FROM users WHERE age = $1 AND name = $2",
    30,
    "alice");

JsonNode? envelope = await conn.QueryAsync<JsonNode>(
    "SELECT * FROM users WHERE age = $1",
    30);

ReadOnlyMemory<byte> nearest = await conn.QueryAsync(
    "SELECT * FROM docs ORDER BY embedding <-> $1 LIMIT 3",
    new float[] { 0.12f, 0.34f, 0.56f });
```

## Query parameters

`QueryAsync(string sql)` is unchanged. Use
`QueryAsync(string sql, params object?[] args)` to bind positional `$N`
placeholders:

```csharp
await conn.QueryAsync("SELECT * FROM users WHERE id = $1", 42);
```

Native mappings:

| .NET value                                      | RedDB value      |
| ----------------------------------------------- | ---------------- |
| `sbyte`, `byte`, `short`, `ushort`, `int`, `uint`, `long` | integer |
| `float`, `double`                               | float            |
| `bool`                                          | bool             |
| `null`, `DBNull.Value`                          | null             |
| `string`                                        | text             |
| `byte[]`                                        | bytes            |
| `float[]`, `ReadOnlyMemory<float>`              | vector           |
| `JsonNode`, `JsonElement`, dictionaries, lists, arrays | json      |
| `DateTime`, `DateTimeOffset`                    | timestamp        |
| `Guid`                                          | uuid             |

## Supported URIs

One URI string covers every transport:

| URI                                   | Transport            |
| ------------------------------------- | -------------------- |
| `red://host[:port]`                   | RedWire (TCP)        |
| `reds://host[:port]`                  | RedWire over TLS     |
| `http://host[:port]`                  | HTTP REST            |
| `https://host[:port]`                 | HTTPS REST           |
| `red://user:pass@host`                | with SCRAM-SHA-256   |
| `red://host?token=sk-abc`             | bearer token         |
| `red://host?apiKey=ak-xyz`            | API key              |

The default port is `5050` for every scheme — matches the engine's
`DEFAULT_REDWIRE_PORT`.

## Auth

* `ConnectOptions.Token` → bearer auth on RedWire and HTTP.
* `ConnectOptions.Username` + `Password` → SCRAM-SHA-256 on RedWire,
  auto `/auth/login` on HTTP.
* No credentials → anonymous (the server must allow it).

## Tests

```bash
dotnet restore drivers/dotnet
dotnet build drivers/dotnet -c Release
dotnet test drivers/dotnet -c Release --nologo
```

Smoke tests are gated on `RED_SMOKE=1` and spawn the real engine via
`cargo run`. Set `RED_BIN=/path/to/red` to reuse an existing binary.

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
