# Bun driver

`@reddb-io/client-bun` is a minimal, ultra-fast RedDB wire client that uses **Bun's native TCP socket** for sub-50µs round-trips. Use it when you run on Bun and need raw throughput on the binary wire protocol; for general Node / Deno / browser apps reach for [`@reddb-io/sdk`](../../guides/javascript-typescript-driver.md) or [`@reddb-io/client`](../../guides/javascript-typescript-driver.md#reddb-ioclient) instead.

- **Package:** npm: [`@reddb-io/client-bun`](https://www.npmjs.com/package/@reddb-io/client-bun)
- **Source:** [`drivers/bun/`](https://github.com/reddb-io/reddb/tree/main/drivers/bun)
- **Status:** Preview
- **Runtime:** Bun only (uses `Bun.connect`)

## Install

```bash
bun add @reddb-io/client-bun
```

## Quickstart

```ts
import { connect } from '@reddb-io/client-bun'

const conn = await connect('127.0.0.1:5050')

const json = await conn.query('SELECT * FROM users LIMIT 10')
console.log(JSON.parse(json))

await conn.bulkInsert('users', [
  JSON.stringify({ name: 'Alice' }),
  JSON.stringify({ name: 'Bob' }),
])

conn.close()
```

`query()` returns the raw JSON string from the server — call `JSON.parse` (or use `queryParsed()`) yourself.

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
- **No URI-level auth.** No SCRAM / OAuth handshake — it speaks a simple `MSG_QUERY` / `MSG_BULK_INSERT` framing on top of plain TCP (or TLS). For full auth + transport feature parity, use [`@reddb-io/client`](../../guides/javascript-typescript-driver.md#reddb-ioclient).
- **Bun-only.** Calls `Bun.connect`. Doesn't run on Node / Deno / browsers.

For a feature-complete JS/TS client (RedWire + HTTP + gRPC + SCRAM/OAuth/mTLS), use [`@reddb-io/client`](../../guides/javascript-typescript-driver.md#reddb-ioclient).

## Production checklist

- Pair with `connectTls()` for cross-network traffic.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for cert / mTLS posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).
