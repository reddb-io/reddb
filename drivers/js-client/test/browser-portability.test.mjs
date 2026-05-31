/**
 * Browser-entry portability guard + functional smoke (#877).
 *
 * The regression net for the failure that motivated PRD #874: a stray `node:`
 * built-in sneaking into the browser import graph. This file statically walks
 * the transitive static-import graph rooted at `src/index.browser.js` and
 * asserts it contains **zero** `node:` specifiers — so a future stray Node
 * import fails CI here, not in a downstream app's bundler. It also proves the
 * browser entry's `connect()` works over HTTP and rejects the non-browser
 * schemes with the documented, actionable errors.
 *
 * Run with: node --test test/*.test.mjs
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  connect,
  RedDB,
  RedDBError,
  EmbeddedNotSupported,
  RowReadable,
  RowWritable,
} from '../src/index.browser.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const SRC = resolve(HERE, '..', 'src')
const BROWSER_ENTRY = resolve(SRC, 'index.browser.js')

// Node-only modules the browser entry must never pull in (directly or
// transitively): the gRPC transport (node:http2), the RedWire transport, and
// the node:stream-based streaming impl.
const FORBIDDEN_MODULES = ['grpc.js', 'redwire.js', 'streaming.js']

/**
 * Extract every static `import ... from '<spec>'`, `export ... from '<spec>'`,
 * and bare `import '<spec>'` specifier from a module's source. Good enough for
 * this codebase (pure ESM, no dynamic import of node: built-ins in the browser
 * path); intentionally conservative — it over-collects rather than missing one.
 */
function importSpecifiers(source) {
  const specs = []
  const re = /(?:^|[\s;])(?:import|export)\b[^'"`]*?from\s*['"]([^'"]+)['"]|(?:^|[\s;])import\s*['"]([^'"]+)['"]/g
  let m
  while ((m = re.exec(source)) !== null) {
    specs.push(m[1] ?? m[2])
  }
  return specs
}

/** Walk the transitive static-import graph from `entry` (absolute path). */
function walkImportGraph(entry) {
  const visited = new Set()
  const nodeBuiltins = []
  const queue = [entry]
  while (queue.length > 0) {
    const file = queue.pop()
    if (visited.has(file)) continue
    visited.add(file)

    const source = readFileSync(file, 'utf8')
    for (const spec of importSpecifiers(source)) {
      if (spec.startsWith('node:')) {
        nodeBuiltins.push({ file, spec })
        continue
      }
      // Only relative specifiers stay inside the package graph. A bare
      // (non-relative, non-node:) specifier would be an external dependency —
      // none are expected; record it so the assertion surfaces it.
      if (spec.startsWith('./') || spec.startsWith('../')) {
        queue.push(resolve(dirname(file), spec))
      } else {
        nodeBuiltins.push({ file, spec: `(bare) ${spec}` })
      }
    }
  }
  return { visited, nodeBuiltins }
}

test('portability: browser entry import graph has zero node: built-ins', () => {
  const { visited, nodeBuiltins } = walkImportGraph(BROWSER_ENTRY)

  assert.deepEqual(
    nodeBuiltins,
    [],
    `browser entry must not import node: built-ins or external deps, found: `
      + JSON.stringify(nodeBuiltins),
  )

  // Sanity: the walk actually traversed the core/http/streaming-web graph.
  assert.ok(visited.size > 5, `expected a multi-module graph, walked ${visited.size}`)
  const names = [...visited].map((f) => f.replace(SRC + '/', ''))
  assert.ok(names.includes('http.js'), 'browser entry should reach http.js')
  assert.ok(names.includes('streaming-web.js'), 'browser entry should reach streaming-web.js')
  assert.ok(names.some((n) => n.startsWith('core/')), 'browser entry should reach the core')
})

test('portability: browser graph excludes the node-only transports', () => {
  const { visited } = walkImportGraph(BROWSER_ENTRY)
  const names = [...visited].map((f) => f.replace(SRC + '/', ''))
  for (const forbidden of FORBIDDEN_MODULES) {
    assert.ok(
      !names.includes(forbidden),
      `browser graph must not include ${forbidden}; walked: ${names.join(', ')}`,
    )
  }
})

test('portability: browser entry exposes the Web-streams row wrappers', () => {
  assert.equal(typeof RowReadable, 'function')
  assert.equal(typeof RowWritable, 'function')
  assert.equal(typeof connect, 'function')
})

// ---------------------------------------------------------------------------
// Functional: connect() over HTTP works from the browser entry.
// ---------------------------------------------------------------------------

function readinessOk() {
  return { ok: true, statement: 'SELECT', affected: 0, columns: ['1'], rows: [{ 1: 1 }] }
}

async function startMockServer(handlers) {
  const defaults = { 'POST /query': () => readinessOk() }
  const server = createServer((req, res) => {
    let body = ''
    req.on('data', (chunk) => { body += chunk })
    req.on('end', async () => {
      const key = `${req.method} ${req.url}`
      const handler = handlers[key] ?? defaults[key]
      if (!handler) {
        res.statusCode = 404
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify({ ok: false, error: 'not found' }))
        return
      }
      try {
        const parsed = body ? JSON.parse(body) : {}
        const out = await handler(parsed, req)
        res.statusCode = out?.status ?? 200
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify(out?.body ?? out))
      } catch (err) {
        res.statusCode = 500
        res.end(JSON.stringify({ ok: false, error: String(err.message || err) }))
      }
    })
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const { port } = server.address()
  return {
    baseUrl: `http://127.0.0.1:${port}`,
    close: () => new Promise((r) => server.close(r)),
  }
}

test('browser connect(http://) returns a RedDB and round-trips query/insert/transaction', async () => {
  const stub = await startMockServer({
    'POST /query': (body) => {
      if (body.query === 'SELECT 1') return readinessOk()
      return { ok: true, statement: 'SELECT', affected: 0, columns: ['n'], rows: [{ n: 42 }] }
    },
    'POST /collections/t/rows': () => ({ ok: true, rid: 7, id: 7, affected: 1 }),
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.ok(db instanceof RedDB)

    const q = await db.query('SELECT n FROM t')
    assert.deepEqual(q.rows, [{ n: 42 }])

    const ins = await db.insert('t', { a: 1 })
    assert.equal(ins.rid, 7)

    const tx = await db.transaction(async (t) => {
      await t.query('SELECT 1')
      return 'ok'
    })
    assert.equal(tx, 'ok')

    // Data-API factories are present (the core's surface, unchanged).
    assert.ok(db.cache && db.queue && db.documents)
    assert.equal(typeof db.kv, 'function')
    assert.equal(typeof db.config, 'function')
    assert.equal(typeof db.vault, 'function')

    await db.close()
  } finally {
    await stub.close()
  }
})

test('browser connect(http://) streams a SELECT over Web streams', async () => {
  const server = createServer((req, res) => {
    let body = ''
    req.on('data', (c) => { body += c })
    req.on('end', () => {
      if (req.url === '/query/stream') {
        res.writeHead(200, { 'content-type': 'application/x-ndjson', 'transfer-encoding': 'chunked' })
        res.write(JSON.stringify({ descriptor: { columns: [{ name: 'id' }] } }) + '\n')
        res.write(JSON.stringify({ row: { id: 1 } }) + '\n')
        res.write(JSON.stringify({ row: { id: 2 } }) + '\n')
        res.end(JSON.stringify({ end: { row_count: 2 } }) + '\n')
        return
      }
      res.writeHead(200, { 'content-type': 'application/json' })
      res.end(JSON.stringify(readinessOk()))
    })
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const { port } = server.address()
  try {
    const db = await connect(`http://127.0.0.1:${port}`)
    const stream = db.stream('SELECT id FROM t')
    assert.ok(stream instanceof RowReadable)
    const rows = []
    for await (const row of stream) rows.push(row)
    assert.deepEqual(rows, [{ id: 1 }, { id: 2 }])
    assert.equal(stream.endInfo.row_count, 2)
    await db.close()
  } finally {
    await new Promise((r) => server.close(r))
  }
})

// ---------------------------------------------------------------------------
// Functional: non-browser schemes raise descriptive, non-crashing errors.
// ---------------------------------------------------------------------------

for (const scheme of ['grpc', 'grpcs', 'red', 'reds']) {
  test(`browser connect(${scheme}://) throws BROWSER_TRANSPORT_UNSUPPORTED`, async () => {
    await assert.rejects(
      connect(`${scheme}://host:5050`),
      (err) => {
        assert.ok(err instanceof RedDBError)
        assert.equal(err.code, 'BROWSER_TRANSPORT_UNSUPPORTED')
        // Names the limitation and the HTTP remedy.
        assert.match(err.message, /browser/i)
        assert.match(err.message, /http\(s\)|https?:\/\//i)
        assert.match(err.message, /gateway|endpoint/i)
        return true
      },
    )
  })
}

test('browser connect(?proto=pg) throws BROWSER_TRANSPORT_UNSUPPORTED', async () => {
  await assert.rejects(
    connect('red://host:5432?proto=pg'),
    (err) => err instanceof RedDBError && err.code === 'BROWSER_TRANSPORT_UNSUPPORTED',
  )
})

// ---------------------------------------------------------------------------
// Functional: embedded URIs rejected with the shared wording.
// ---------------------------------------------------------------------------

for (const uri of ['memory://', 'file:///tmp/db', 'red:///var/data', 'red://:memory:']) {
  test(`browser connect(${uri}) rejects embedded with the shared wording`, async () => {
    await assert.rejects(
      connect(uri),
      (err) => {
        assert.ok(err instanceof EmbeddedNotSupported)
        assert.match(err.message, /embedded schemes/)
        return true
      },
    )
  })
}
