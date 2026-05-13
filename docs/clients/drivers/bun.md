# Bun driver

`@reddb-io/client-bun` is a minimal, ultra-fast RedDB wire client that uses **Bun's native TCP socket** for sub-50µs round-trips. Use it when you run on Bun and need raw throughput on the binary wire protocol; for general Node / Deno / browser apps reach for [`@reddb-io/sdk`](../../guides/javascript-typescript-driver.md) or [`@reddb-io/client`](../../guides/javascript-typescript-driver.md#reddb-ioclient) instead.

- **Package:** npm: [`@reddb-io/client-bun`](https://www.npmjs.com/package/@reddb-io/client-bun)
- **Source:** [`drivers/bun/`](https://github.com/reddb-io/reddb/tree/main/drivers/bun)
- **Status:** Preview
- **Runtime:** Bun only (uses `Bun.connect`)

## Install

```bash
bun add @reddb-io/sdk @reddb-io/client-bun
```

## Quickstart

```ts
import { connect as connectSdk } from '@reddb-io/sdk'
import { connect as connectNative } from '@reddb-io/client-bun'

const db = await connectSdk('memory://')

const rows = await db.query(
  'SELECT * FROM users WHERE name = $1',
  ['Alice'],
)
console.log(rows.rows)

await db.close()

const conn = await connectNative('127.0.0.1:5050')
const json = await conn.query('SELECT * FROM users WHERE name = $1', 'Alice')
console.log(JSON.parse(json))

conn.close()
```

`query()` returns the raw JSON string from the server — call `JSON.parse` (or use `queryParsed()`) yourself.

## Safe parameter binding

Use positional `$N` bind values for user input. The shared `@reddb-io/sdk`
accepts params arrays, and the native `@reddb-io/client-bun` TCP client accepts
either variadic params or a single params array. The cross-driver contract is
tracked in [ADR #352](https://github.com/reddb-io/reddb/issues/352).

```ts
import { connect } from '@reddb-io/sdk'
import { connect as connectNative } from '@reddb-io/client-bun'

const db = await connect('memory://')

const rows = await db.query(
  'SELECT id, name FROM users WHERE id = $1 AND tenant = $2',
  [42, 'acme'],
)

const hits = await db.query(
  'SEARCH SIMILAR $1 IN embeddings K 5',
  [Float32Array.from([0.1, 0.2, 0.3])],
)

const conn = await connectNative('127.0.0.1:5050')
const raw = await conn.query(
  'SELECT id, name FROM users WHERE id = $1 AND tenant = $2',
  42,
  'acme',
)
const parsed = JSON.parse(raw)
```

## TLS

```ts
import { connectTls } from '@reddb-io/client-bun'

const conn = await connectTls('reddb.example.com:5050', {
  ca: await Bun.file('/etc/ca.pem').text(),
  rejectUnauthorized: true,
})
```

## Scope and trade-offs

The Bun driver is intentionally narrow:

- **Address format is `host:port`** — not a full `red://` URI. Pair it with [`URL`](https://developer.mozilla.org/docs/Web/API/URL) yourself if you need parsing.
- **Parameterized embedded queries use `@reddb-io/sdk`.** The shared SDK is verified under Bun for `db.query(sql, params)`, including vector params. The native `@reddb-io/client-bun` TCP path uses `QueryWithParams` for non-empty params when the server advertises `FEATURE_PARAMS`.
- **No URI-level auth.** No SCRAM / OAuth handshake — it speaks a simple `MSG_QUERY` / `MSG_BULK_INSERT` framing on top of plain TCP (or TLS). For full auth + transport feature parity, use [`@reddb-io/client`](../../guides/javascript-typescript-driver.md#reddb-ioclient).
- **Bun-only.** Calls `Bun.connect`. Doesn't run on Node / Deno / browsers.

For a feature-complete JS/TS client (RedWire + HTTP + gRPC + SCRAM/OAuth/mTLS), use [`@reddb-io/client`](../../guides/javascript-typescript-driver.md#reddb-ioclient).

## Test

```bash
cargo build --bin red
cd drivers/bun
bun run params.test.ts
```

## Production checklist

- Pair with `connectTls()` for cross-network traffic.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for cert / mTLS posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).
