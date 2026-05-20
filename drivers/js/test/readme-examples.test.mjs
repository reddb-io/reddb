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

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed ? 1 : 0)
