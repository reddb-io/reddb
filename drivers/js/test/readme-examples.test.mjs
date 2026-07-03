/**
 * Executes the runnable code examples published in README.md against a real
 * `memory://` engine, so the docs can't drift from the API (acceptance for
 * issue #559: "Examples in driver README have automated tests").
 *
 * Each block below mirrors a fenced example in README.md. Self-skips with exit
 * 0 when the binary is absent (same contract as smoke.test.mjs).
 */

import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

import { connect, RedDBError } from '../src/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const DEFAULT_BINARY = resolve(HERE, '..', '..', '..', 'target', 'debug', 'red')
const BINARY = process.env.REDDB_BINARY_PATH || DEFAULT_BINARY

if (!existsSync(BINARY)) {
  console.error(`SKIP: binary not found at ${BINARY}`)
  console.error('Run "cargo build" first or set REDDB_BINARY_PATH.')
  process.exit(0)
}

let passed = 0
let failed = 0

async function example(name, fn) {
  const db = await connect('memory://', { binary: BINARY })
  try {
    await fn(db)
    console.log(`  ok  ${name}`)
    passed++
  } catch (err) {
    console.error(`  FAIL ${name}\n        ${err.stack || err.message}`)
    failed++
  } finally {
    await db.close()
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(`assertion failed: ${msg}`)
}

// README "Quickstart"
await example('readme: quickstart', async (db) => {
  await db.insert('users', { name: 'Alice', age: 30 })
  await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])

  const result = await db.query('SELECT * FROM users')
  assert(Array.isArray(result.rows), 'query returns rows')

  const doc = await db.documents.insert('events', { event_type: 'login', attempts: 1 })
  const patched = await db.documents.patch('events', doc.rid, { reviewed: true })
  assert(patched.event_type === 'login', 'patch preserves unrelated fields')

  await db.query('CREATE KV settings')
  const kv = db.kv('settings')
  await kv.put('characters:hansel', 'crumbs')
  assert((await kv.get('characters:hansel')) === 'crumbs', 'kv round trip')
})

// README "db.documents"
await example('readme: documents helpers', async (db) => {
  const inserted = await db.documents.insert('events', {
    event_type: 'login',
    details: { ip: '10.0.0.7' },
  })
  const event = await db.documents.get('events', inserted.rid)
  assert(event.event_type === 'login', 'document get')
  const page = await db.documents.list('events', { filter: "event_type = 'login'", limit: 10 })
  assert(Array.isArray(page.items), 'document list')
  const updated = await db.documents.patch('events', inserted.rid, { reviewed: true })
  assert(updated.event_type === 'login', 'patch merge')
  const del = await db.documents.delete('events', inserted.rid)
  assert(del.deleted === true, 'delete returns { affected, deleted }')
})

// README "db.kv"
await example('readme: kv helpers', async (db) => {
  await db.query('CREATE KV settings')
  const kv = db.kv('settings')
  await kv.set('characters:hansel', 'crumbs')
  assert((await kv.get('characters:hansel')) === 'crumbs', 'kv get')
  assert((await kv.exists('characters:hansel')).exists === true, 'kv exists')
  const list = await kv.list({ prefix: 'characters:' })
  assert(list.items.some((row) => row.key === 'characters:hansel'), 'kv list prefix')
  const del = await kv.delete('characters:hansel')
  assert(del.deleted === true, 'kv delete returns { affected, deleted }')
})

// README "db.queues"
await example('readme: queue helpers', async (db) => {
  await db.queues.create('jobs')
  await db.queues.push('jobs', { task: 'ship' })
  assert((await db.queues.peek('jobs')).length >= 1, 'queue peek')
  assert((await db.queues.len('jobs')) === 1, 'queue len')
  assert((await db.queues.pop('jobs')).length === 1, 'queue pop')
  await db.queues.purge('jobs')
  assert((await db.queues.len('jobs')) === 0, 'queue purge resets len')
})

// README "Transactions" — imperative and callback forms
await example('readme: transactions', async (db) => {
  await db.query('CREATE TABLE audit (action TEXT)')

  const rid = await db.transaction(async (tx) => {
    const inserted = await tx.insert('tx_users', { name: 'Ada' })
    await tx.query('INSERT INTO audit (action) VALUES ($1)', 'created user')
    return inserted.rid
  })
  assert(rid != null, 'callback transaction returns value')

  const tx = db.tx()
  await tx.begin()
  await db.query("INSERT INTO audit (action) VALUES ('imperative')")
  await tx.commit()
  const r = await db.query("SELECT action FROM audit WHERE action = 'imperative'")
  assert((r.rows ?? []).length === 1, 'imperative commit persists')
})

// README "Errors"
await example('readme: errors', async (db) => {
  let raised = null
  try {
    await db.query('NOT VALID SQL')
  } catch (err) {
    raised = err
  }
  assert(raised instanceof RedDBError, 'invalid SQL throws RedDBError')
  assert(typeof raised.code === 'string', 'error exposes a code')
})

// README "Graph, vector and time-series" — graph traversal
await example('readme: graph traversal', async (db) => {
  // The first user-inserted item in a fresh database gets rid 1024
  // (1..1023 are reserved), so these three nodes are 1024, 1025, 1026.
  await db.query("INSERT INTO network NODE (label, node_type) VALUES ('gateway', 'Host')")
  await db.query("INSERT INTO network NODE (label, node_type) VALUES ('app', 'Host')")
  await db.query("INSERT INTO network NODE (label, node_type) VALUES ('db', 'Host')")
  await db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1025, 1.0)")
  await db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1025, 1026, 1.0)")
  await db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1026, 5.0)")

  const path = await db.query("GRAPH SHORTEST_PATH '1024' TO '1026' ALGORITHM dijkstra")
  assert(Array.isArray(path.rows), 'graph shortest_path returns rows')
  // Dijkstra prefers the two 1.0-weight hops over the single 5.0 edge.
  assert(path.rows[0].total_weight === 2, 'cheapest path totals weight 2')
})

// README "Graph, vector and time-series" — vector search
await example('readme: vector search', async (db) => {
  await db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway runbook')")
  await db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database manual')")

  // A vector bind value is a single $N param, so it goes in the params
  // array as one element: [[1.0, 0.0]], not the variadic form.
  const hits = await db.query('SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1', [[1.0, 0.0]])
  assert(hits.rows.length === 1, 'vector search returns the nearest row')
  assert(hits.rows[0].score === 1, 'the identical vector scores exactly 1')
})

// README "Graph, vector and time-series" — time-series rollup
await example('readme: timeseries rollup', async (db) => {
  await db.query('CREATE TIMESERIES metrics RETENTION 7 d CHUNK_SIZE 64 DOWNSAMPLE 1h:5m:avg')
  await db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 10.0, '{\"host\":\"srv-a\"}', 0)")
  await db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 20.0, '{\"host\":\"srv-a\"}', 60000000000)")
  await db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 30.0, '{\"host\":\"srv-b\"}', 300000000000)")

  const rollup = await db.query(
    'SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples ' +
      "FROM metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m)",
  )
  assert(rollup.rows.length === 2, 'two five-minute buckets')
})

// README "Isolation levels" — BEGIN ISOLATION LEVEL ... via raw query
await example('readme: isolation level', async (db) => {
  await db.query('CREATE TABLE audit (action TEXT)')
  await db.query('BEGIN ISOLATION LEVEL SERIALIZABLE')
  await db.query("INSERT INTO audit (action) VALUES ('serializable write')")
  await db.query('COMMIT')
  const r = await db.query('SELECT action FROM audit')
  assert((r.rows ?? []).length === 1, 'serializable transaction commits')
})

// README "Serialization conflicts & retries" — classifier + retry loop
await example('readme: serialization retry', async (db) => {
  // A serialization conflict surfaces as a RedDBError with code
  // QUERY_ERROR whose message begins "serialization conflict".
  const isSerializationConflict = (err) =>
    err instanceof RedDBError &&
    err.code === 'QUERY_ERROR' &&
    /serialization conflict/i.test(err.message)

  async function withRetry(fn, { maxRetries = 5 } = {}) {
    for (let attempt = 0; ; attempt++) {
      try {
        return await db.transaction(fn)
      } catch (err) {
        if (isSerializationConflict(err) && attempt < maxRetries) {
          await new Promise((r) => setTimeout(r, 2 ** attempt * 5))
          continue
        }
        throw err
      }
    }
  }

  await db.query('CREATE TABLE ledger (entry TEXT)')
  const result = await withRetry(async (tx) => {
    await tx.query("INSERT INTO ledger (entry) VALUES ('committed')")
    return 'ok'
  })
  assert(result === 'ok', 'retry loop runs the transaction to commit')

  // The classifier only fires for real serialization conflicts.
  assert(
    isSerializationConflict(
      new RedDBError('QUERY_ERROR', 'serialization conflict: table row accounts/1 was modified by concurrent transaction 42'),
    ),
    'classifies a serialization conflict',
  )
  assert(
    !isSerializationConflict(new RedDBError('QUERY_ERROR', 'syntax error near FROM')),
    'ignores unrelated query errors',
  )
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed ? 1 : 0)
