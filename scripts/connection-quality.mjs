#!/usr/bin/env node
// RedDB connection-quality harness.
//
// Measures the *transport edge* quality of a running RedDB server, not the
// query engine: request latency distribution, sequential and concurrent
// throughput, the HTTP connection-limiter / 503 behaviour under load, and the
// before/after of the async-connection-model migration (ADR 0036, PRD #930).
//
// Three transports are measured side by side so the migration's before/after
// is one table:
//   * HTTP over raw TCP (node:net) — driver-independent. Compares "one TCP
//     connection per request" (the v0 `Connection: close` cost) against
//     "reuse one connection for a burst" (free once the edge is async).
//   * RedWire-over-TCP — the native binary protocol, via the local js-client
//     (`--wire host:port`), the throughput ceiling for native drivers.
//   * RedWire-over-WSS — the *browser* transport (#935/#937, ADR 0036): the
//     exact RedWire frame protocol tunneled over a binary WebSocket
//     (`--ws wss://host:port/redwire`). Same multiplexed binary protocol as
//     native drivers, measured the same way.
//
// Usage:
//   node scripts/connection-quality.mjs --http 127.0.0.1:5000 \
//        [--wire 127.0.0.1:5050] [--ws wss://127.0.0.1:55555/redwire] \
//        [--token <bearer>] [--requests 2000] [--concurrency 64] [--sql "SELECT 1"]
//
// WSS notes: the browser endpoint is WSS-only and Origin-gated (ADR 0036), so
// `--ws` needs a `wss://` URL on a TLS edge whose `web.websocket_allowed_origins`
// admits this client. For a self-signed loopback cert, run the harness under
// `NODE_TLS_REJECT_UNAUTHORIZED=0` (or pass `--ws-insecure`, which sets it).
//
// Exit code is 0 on a completed run regardless of the measured numbers. An
// unreachable `--wire`/`--ws` is *skipped* (with a note), not fatal — only an
// unreachable `--http` is fatal, since it is the always-present baseline.

import net from 'node:net'
import { fileURLToPath } from 'node:url'

// ---- args -------------------------------------------------------------------

export function parseArgs(argv) {
  const out = {
    http: '127.0.0.1:5000',
    wire: null,
    ws: null,
    wsInsecure: false,
    token: null,
    requests: 2000,
    concurrency: 64,
    sql: 'SELECT 1',
    field: 'sql', // current server contract; legacy 0.1.x used 'query'
  }
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i]
    const next = () => argv[++i]
    if (a === '--http') out.http = next()
    else if (a === '--wire') out.wire = next()
    else if (a === '--ws' || a === '--wss') out.ws = next()
    else if (a === '--ws-insecure') out.wsInsecure = true
    else if (a === '--token') out.token = next()
    else if (a === '--requests') out.requests = parseInt(next(), 10)
    else if (a === '--concurrency') out.concurrency = parseInt(next(), 10)
    else if (a === '--sql') out.sql = next()
    else if (a === '--field') out.field = next()
    else if (a === '--help' || a === '-h') {
      console.log(
        'usage: connection-quality.mjs --http host:port [--wire host:port] ' +
          '[--ws wss://host:port/redwire] [--ws-insecure] [--token t] ' +
          '[--requests N] [--concurrency C] [--sql "..."]',
      )
      process.exit(0)
    }
  }
  return out
}

function hostPort(s) {
  const i = s.lastIndexOf(':')
  return { host: s.slice(0, i) || '127.0.0.1', port: parseInt(s.slice(i + 1), 10) }
}

// Normalize a `--ws` value into a `wss://host:port/redwire` URL. Accepts a
// bare `host:port` (defaults the scheme and the `/redwire` route the server
// mounts), or a full `wss://…`/`ws://…` URL passed through verbatim.
export function normalizeWssUrl(s) {
  if (/^wss?:\/\//i.test(s)) return s
  const { host, port } = hostPort(s)
  return `wss://${host}:${port}/redwire`
}

// ---- stats ------------------------------------------------------------------

function percentile(sortedMs, p) {
  if (sortedMs.length === 0) return NaN
  const idx = Math.min(sortedMs.length - 1, Math.floor((p / 100) * sortedMs.length))
  return sortedMs[idx]
}

export function summarize(label, samplesMs, wallMs, extra = {}) {
  const s = [...samplesMs].sort((a, b) => a - b)
  const n = s.length
  const mean = n ? s.reduce((a, b) => a + b, 0) / n : NaN
  return {
    label,
    n,
    p50: percentile(s, 50),
    p90: percentile(s, 90),
    p99: percentile(s, 99),
    max: n ? s[n - 1] : NaN,
    mean,
    throughput: wallMs > 0 ? (n / wallMs) * 1000 : NaN,
    ...extra,
  }
}

function fmt(row) {
  const ms = (x) => (Number.isFinite(x) ? x.toFixed(3) : '   -  ')
  return (
    `${row.label.padEnd(34)} ` +
    `n=${String(row.n).padStart(5)}  ` +
    `p50=${ms(row.p50)}  p90=${ms(row.p90)}  p99=${ms(row.p99)}  max=${ms(row.max)} ms  ` +
    `${Number.isFinite(row.throughput) ? row.throughput.toFixed(0).padStart(6) : '   -  '} req/s` +
    (row.note ? `  ${row.note}` : '')
  )
}

// ---- raw HTTP ---------------------------------------------------------------

// Build a single HTTP/1.1 POST /query request. `connClose` forces the client to
// advertise `Connection: close`; otherwise we leave HTTP/1.1's persistent
// default so the server's keep-alive decision governs.
let REQUEST_FIELD = 'sql'
function buildRequest({ host, port, token, sql, connClose }) {
  const body = JSON.stringify({ [REQUEST_FIELD]: sql })
  const headers = [
    `POST /query HTTP/1.1`,
    `Host: ${host}:${port}`,
    `Content-Type: application/json`,
    `Content-Length: ${Buffer.byteLength(body)}`,
  ]
  if (token) headers.push(`Authorization: Bearer ${token}`)
  if (connClose) headers.push('Connection: close')
  return Buffer.from(headers.join('\r\n') + '\r\n\r\n' + body)
}

// Read one HTTP response from a socket buffer state. Returns {status, keepAlive,
// bytesConsumed} once a full response (headers + Content-Length body) is
// present in `buf`, else null.
function tryParseResponse(buf) {
  const headerEnd = buf.indexOf('\r\n\r\n')
  if (headerEnd === -1) return null
  const head = buf.slice(0, headerEnd).toString('latin1')
  const statusMatch = head.match(/^HTTP\/1\.1 (\d{3})/)
  const status = statusMatch ? parseInt(statusMatch[1], 10) : 0
  const clMatch = head.match(/\r\nContent-Length:\s*(\d+)/i)
  const contentLength = clMatch ? parseInt(clMatch[1], 10) : 0
  const total = headerEnd + 4 + contentLength
  if (buf.length < total) return null
  const keepAlive = /\r\nConnection:\s*keep-alive/i.test(head)
  return { status, keepAlive, bytesConsumed: total }
}

// One request on its own fresh TCP connection (the v0 Connection: close cost
// model). Resolves with {ms, status}.
function oneRequestOwnConnection({ host, port, token, sql }) {
  return new Promise((resolve, reject) => {
    const start = process.hrtime.bigint()
    const sock = net.connect({ host, port })
    let buf = Buffer.alloc(0)
    let settled = false
    const done = (err, value) => {
      if (settled) return
      settled = true
      sock.destroy()
      if (err) reject(err)
      else resolve(value)
    }
    sock.setNoDelay(true)
    sock.on('connect', () => sock.write(buildRequest({ host, port, token, sql, connClose: true })))
    sock.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      const r = tryParseResponse(buf)
      if (r) {
        const ms = Number(process.hrtime.bigint() - start) / 1e6
        done(null, { ms, status: r.status })
      }
    })
    sock.on('error', (e) => done(e))
    sock.on('close', () => done(new Error('closed before response')))
    sock.setTimeout(15000, () => done(new Error('timeout')))
  })
}

// `count` requests reusing a single TCP connection (keep-alive burst). If the
// server closes after a response (no keep-alive), this falls back to reopening
// the connection — so it still completes, but reveals whether keep-alive is on
// via the returned `serverKeepAlive` flag.
function burstSingleConnection({ host, port, token, sql, count }) {
  return new Promise((resolve, reject) => {
    const samples = []
    let serverKeepAlive = false
    let reopens = 0
    let done = 0
    let sock = null
    let buf = Buffer.alloc(0)
    let reqStart = 0n
    let finished = false

    const finish = (err) => {
      if (finished) return
      finished = true
      if (sock) sock.destroy()
      if (err) reject(err)
      else resolve({ samples, serverKeepAlive, reopens })
    }

    const sendNext = () => {
      if (done >= count) return finish()
      reqStart = process.hrtime.bigint()
      sock.write(buildRequest({ host, port, token, sql, connClose: false }))
    }

    // Reopen at most once per pending request — a runaway reopen loop (e.g. a
    // refused port) would otherwise spin; bail with the underlying error.
    const reopen = (err) => {
      if (finished) return
      if (done >= count) return finish()
      if (reopens > count + 8) return finish(err ?? new Error('too many reopens'))
      reopens++
      open()
    }

    const open = () => {
      buf = Buffer.alloc(0)
      const thisSock = net.connect({ host, port })
      sock = thisSock
      // Events can still fire on a *previous* socket after we've moved on
      // (a `Connection: close` server often sends RST right after the body);
      // ignore anything not from the current socket.
      const isCurrent = () => sock === thisSock
      thisSock.setNoDelay(true)
      thisSock.on('connect', () => {
        if (isCurrent()) sendNext()
      })
      thisSock.on('data', (d) => {
        if (!isCurrent()) return
        buf = Buffer.concat([buf, d])
        const r = tryParseResponse(buf)
        if (!r) return
        samples.push(Number(process.hrtime.bigint() - reqStart) / 1e6)
        serverKeepAlive = serverKeepAlive || r.keepAlive
        buf = buf.slice(r.bytesConsumed)
        done++
        if (r.keepAlive && done < count) {
          sendNext() // reuse the live connection
        } else if (done < count) {
          thisSock.destroy()
          reopen() // server closed: reopen (this IS the cost keep-alive removes)
        } else {
          finish()
        }
      })
      thisSock.on('error', (e) => {
        // A reset/EPIPE on the current socket after a `Connection: close`
        // reply is the expected close, not a failure: reopen and keep going.
        if (isCurrent()) reopen(e)
      })
      thisSock.on('close', () => {
        if (isCurrent() && !finished && done < count) reopen()
      })
      thisSock.setTimeout(15000, () => {
        if (isCurrent()) finish(new Error('timeout'))
      })
    }
    open()
  })
}

// Concurrency / 503 probe: fire `concurrency` simultaneous fresh-connection
// requests, repeated `rounds` times. Reports the 200/503/other split — the
// HttpConnectionLimiter behaviour under load.
async function concurrencyProbe({ host, port, token, sql, concurrency, rounds }) {
  let ok = 0
  let busy = 0
  let other = 0
  let errors = 0
  const lat = []
  for (let r = 0; r < rounds; r++) {
    const batch = Array.from({ length: concurrency }, () =>
      oneRequestOwnConnection({ host, port, token, sql })
        .then(({ ms, status }) => {
          lat.push(ms)
          if (status === 200) ok++
          else if (status === 503) busy++
          else other++
        })
        .catch(() => errors++),
    )
    await Promise.all(batch)
  }
  return { ok, busy, other, errors, lat }
}

// ---- RedWire (TCP and WSS share the codec, differ only in byte transport) ---

// Sequential request loop over any object exposing the RedWire client surface
// (`.call('query', { sql })`). Used for both RedWire-over-TCP and
// RedWire-over-WSS so the two transports are measured by identical code — the
// only difference is which byte stream carries the frames (ADR 0036: the frame
// protocol is decoupled from the transport). Exported for unit testing against
// a mock client.
export async function measureRedwireSeq(client, sql, requests) {
  const samples = []
  const t0 = process.hrtime.bigint()
  for (let i = 0; i < requests; i++) {
    const s = process.hrtime.bigint()
    await client.call('query', { sql })
    samples.push(Number(process.hrtime.bigint() - s) / 1e6)
  }
  const wall = Number(process.hrtime.bigint() - t0) / 1e6
  return { samples, wall }
}

// Concurrency probe for a multiplexed RedWire connection: fire `concurrency`
// in-flight queries on the *same* connection (stream multiplexing), repeated
// `rounds` times. Unlike the HTTP probe this opens no extra connections — it
// measures the multiplexed-stream concurrency that the browser path gets for
// free over one WSS socket. Exported for unit testing against a mock client.
export async function measureRedwireConcurrency(client, sql, concurrency, rounds) {
  const lat = []
  let ok = 0
  let errors = 0
  for (let r = 0; r < rounds; r++) {
    const batch = Array.from({ length: concurrency }, () => {
      const s = process.hrtime.bigint()
      return client
        .call('query', { sql })
        .then(() => {
          lat.push(Number(process.hrtime.bigint() - s) / 1e6)
          ok++
        })
        .catch(() => errors++)
    })
    await Promise.all(batch)
  }
  return { ok, errors, lat }
}

// Open a RedWire-over-WSS client via the js-client browser transport. Uses the
// runtime's global `WebSocket` (Node 22+ ships one); the server enforces
// WSS-only + Origin allowlist on its side. Returns the connected client, or
// throws — callers treat a throw as "skip this transport".
async function connectWss({ url, token, insecure }) {
  if (insecure) process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'
  if (typeof globalThis.WebSocket !== 'function') {
    throw new Error('no global WebSocket in this Node runtime (need Node 22+)')
  }
  const { connectRedwireWs } = await import('../drivers/js-client/src/redwire-ws.js')
  const auth = token ? { kind: 'bearer', token } : { kind: 'anonymous' }
  return await connectRedwireWs({ url, auth })
}

// ---- runner -----------------------------------------------------------------

async function run() {
  const args = parseArgs(process.argv)
  REQUEST_FIELD = args.field
  const http = hostPort(args.http)
  const wsUrl = args.ws ? normalizeWssUrl(args.ws) : null
  console.log(`# RedDB connection-quality harness`)
  console.log(
    `# http=${args.http} wire=${args.wire ?? '(skipped)'} ws=${wsUrl ?? '(skipped)'} ` +
      `requests=${args.requests} concurrency=${args.concurrency} sql=${JSON.stringify(args.sql)}\n`,
  )

  // Warm-up + reachability.
  try {
    const probe = await oneRequestOwnConnection({ ...http, token: args.token, sql: args.sql })
    console.log(`reachable: HTTP ${args.http} → status ${probe.status} (${probe.ms.toFixed(2)} ms first byte→full)\n`)
  } catch (e) {
    console.error(`✗ HTTP ${args.http} unreachable: ${e.message}`)
    process.exit(1)
  }

  const rows = []

  // 1) Sequential latency, one connection per request (Connection: close cost).
  {
    const samples = []
    const t0 = process.hrtime.bigint()
    for (let i = 0; i < args.requests; i++) {
      const { ms } = await oneRequestOwnConnection({ ...http, token: args.token, sql: args.sql })
      samples.push(ms)
    }
    const wall = Number(process.hrtime.bigint() - t0) / 1e6
    rows.push(summarize('HTTP seq · new conn/req', samples, wall, { note: 'Connection: close baseline' }))
  }

  // 2) Sequential latency, reusing one connection (keep-alive burst).
  {
    const t0 = process.hrtime.bigint()
    const { samples, serverKeepAlive, reopens } = await burstSingleConnection({
      ...http,
      token: args.token,
      sql: args.sql,
      count: args.requests,
    })
    const wall = Number(process.hrtime.bigint() - t0) / 1e6
    rows.push(
      summarize('HTTP seq · reuse conn', samples, wall, {
        note: serverKeepAlive
          ? `keep-alive ON (reopens=${reopens})`
          : `keep-alive OFF — server closed each time (reopens=${reopens})`,
      }),
    )
  }

  // 3) RedWire-over-TCP, if requested and the driver is reachable.
  if (args.wire) {
    try {
      const { connect } = await import('../drivers/js-client/src/index.js')
      const wire = hostPort(args.wire)
      const uri = `red://${wire.host}:${wire.port}`
      const db = await connect(uri, args.token ? { auth: { token: args.token } } : {})
      const samples = []
      const t0 = process.hrtime.bigint()
      for (let i = 0; i < args.requests; i++) {
        const s = process.hrtime.bigint()
        await db.query(args.sql)
        samples.push(Number(process.hrtime.bigint() - s) / 1e6)
      }
      const wall = Number(process.hrtime.bigint() - t0) / 1e6
      rows.push(summarize('RedWire-over-TCP seq · mux conn', samples, wall, { note: 'binary TCP' }))
      if (typeof db.close === 'function') await db.close()
    } catch (e) {
      console.error(`! RedWire-over-TCP skipped: ${e.message}`)
    }
  }

  // 4) RedWire-over-WSS — the browser transport (#935/#937, ADR 0036). Same
  //    codec as TCP, only the byte stream differs, so it is measured by the
  //    same `measureRedwireSeq` loop and prints on the same table.
  let wsClient = null
  if (wsUrl) {
    try {
      wsClient = await connectWss({ url: wsUrl, token: args.token, insecure: args.wsInsecure })
      const { samples, wall } = await measureRedwireSeq(wsClient, args.sql, args.requests)
      rows.push(summarize('RedWire-over-WSS seq · mux conn', samples, wall, { note: 'binary WebSocket (browser)' }))
    } catch (e) {
      console.error(
        `! RedWire-over-WSS skipped: ${e.message}` +
          ` (needs a wss:// TLS edge with web.websocket_allowed_origins admitting this client;` +
          ` for a self-signed cert run with --ws-insecure)`,
      )
      wsClient = null
    }
  }

  // Print latency/throughput table.
  console.log('## latency & throughput')
  for (const row of rows) console.log(fmt(row))
  console.log()

  // 5) Concurrency / 503 probe (HTTP connection limiter).
  const rounds = 5
  const probe = await concurrencyProbe({
    ...http,
    token: args.token,
    sql: args.sql,
    concurrency: args.concurrency,
    rounds,
  })
  const total = probe.ok + probe.busy + probe.other + probe.errors
  const lat = summarize(`HTTP concurrency=${args.concurrency}`, probe.lat, 0)
  console.log(`## connection-limiter probe (concurrency=${args.concurrency} × ${rounds} rounds = ${total} reqs)`)
  console.log(
    `  HTTP: 200=${probe.ok}  503=${probe.busy}  other=${probe.other}  conn-errors=${probe.errors}` +
      `   p50=${lat.p50.toFixed(2)} p99=${lat.p99.toFixed(2)} max=${lat.max.toFixed(2)} ms`,
  )
  console.log(
    `  → ${probe.busy > 0 || probe.errors > 0
      ? `cap engaged: ${probe.busy} load-shed 503 + ${probe.errors} refused (HttpConnectionLimiter working as designed)`
      : 'no shedding at this concurrency — raise --concurrency to find the cap'}`,
  )

  // 6) RedWire-over-WSS concurrency — in-flight multiplexed streams on ONE
  //    socket. The browser path needs no extra connections to fan out, so
  //    there is no per-connection cap to engage; this records the multiplexed
  //    concurrency latency for the after-migration record.
  if (wsClient) {
    try {
      const c = await measureRedwireConcurrency(wsClient, args.sql, args.concurrency, rounds)
      const clat = summarize(`WSS mux concurrency=${args.concurrency}`, c.lat, 0)
      console.log(
        `  WSS (1 socket, multiplexed): ok=${c.ok}  errors=${c.errors}` +
          `   p50=${clat.p50.toFixed(2)} p99=${clat.p99.toFixed(2)} max=${clat.max.toFixed(2)} ms` +
          ` — no per-connection cap to engage (multiplexed over one socket)`,
      )
    } catch (e) {
      console.error(`! WSS concurrency probe skipped: ${e.message}`)
    } finally {
      if (typeof wsClient.close === 'function') await wsClient.close()
    }
  }
}

// Only auto-run when invoked as a script; importing the module (for tests)
// must not kick off a measurement run.
const invokedDirectly =
  process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]
if (invokedDirectly) {
  run().catch((e) => {
    console.error(e)
    process.exit(1)
  })
}
