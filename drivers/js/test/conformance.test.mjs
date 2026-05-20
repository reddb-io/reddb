/**
 * SDK Helper Spec — conformance harness (JS driver).
 *
 * Ports the reference case list from `docs/spec/sdk-helpers.md` §12 (mirroring
 * the Rust harness at `crates/reddb-client/tests/conformance.rs`). Case IDs are
 * preserved verbatim (dots kept in the test label) so cross-driver CI
 * dashboards line up.
 *
 * The JS SDK embeds the engine via `red rpc --stdio`, so the harness runs every
 * case against a fresh `memory://` connection using the locally built binary.
 * It self-skips with exit 0 when the binary is absent so CI on machines without
 * a prior `cargo build` is not blocked (same contract as smoke.test.mjs).
 *
 * Run:
 *   cargo build                              # produces target/debug/red
 *   node drivers/js/test/conformance.test.mjs
 *   # or: REDDB_BINARY_PATH=/path/to/red node drivers/js/test/conformance.test.mjs
 */

import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

import { connect, RedDBError, HELPER_SPEC_VERSION } from '../src/index.js'

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

async function test(caseId, fn) {
  const db = await connect('memory://', { binary: BINARY })
  try {
    await fn(db)
    console.log(`  ok  ${caseId}`)
    passed++
  } catch (err) {
    console.error(`  FAIL ${caseId}\n        ${err.stack || err.message}`)
    failed++
  } finally {
    await db.close()
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(`assertion failed: ${msg}`)
}

function assertEqual(actual, expected, msg) {
  if (actual !== expected) {
    throw new Error(`${msg}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`)
  }
}

async function expectCode(fn, code, msg) {
  let raised = null
  try {
    await fn()
  } catch (err) {
    raised = err
  }
  assert(raised != null, `${msg}: expected to raise`)
  assert(raised instanceof RedDBError, `${msg}: expected RedDBError, got ${raised}`)
  assertEqual(raised.code, code, `${msg}: error code`)
}

// --- meta.* -----------------------------------------------------------

await test('meta.spec_version', async (db) => {
  // Spec §14: every driver exposes the helper spec version it implements.
  assertEqual(HELPER_SPEC_VERSION, '1.0', 'HELPER_SPEC_VERSION constant')
  assertEqual(db.helperSpecVersion, '1.0', 'db.helperSpecVersion')
})

// --- generic.* --------------------------------------------------------

await test('generic.query.no_params', async (db) => {
  await db.query('CREATE TABLE conf_q (id INTEGER, name TEXT)')
  await db.query("INSERT INTO conf_q (id, name) VALUES (1, 'a')")
  const r = await db.query('SELECT id, name FROM conf_q')
  assert(Array.isArray(r.rows) && r.rows.some((row) => row.name === 'a'), 'row round trip')
})

await test('generic.query_with.params', async (db) => {
  await db.query('CREATE TABLE conf_p (id INTEGER, name TEXT)')
  await db.query('INSERT INTO conf_p (id, name) VALUES ($1, $2)', 42, 'alice')
  const r = await db.query('SELECT name FROM conf_p WHERE id = $1', 42)
  assertEqual(r.rows?.[0]?.name, 'alice', 'param-bound select')
})

await test('generic.insert.rid', async (db) => {
  const r = await db.insert('conf_ins', { name: 'eve' })
  assertEqual(r.affected, 1, 'affected = 1')
  assert(r.rid != null && String(r.rid).length > 0, 'lossless rid present')
})

await test('generic.bulk_insert.rids', async (db) => {
  // Touch the collection first.
  await db.insert('conf_bulk', { seed: true })
  const empty = await db.bulkInsert('conf_bulk', [])
  assertEqual(empty.affected, 0, 'empty bulk affected = 0')
  assert(Array.isArray(empty.rids) && empty.rids.length === 0, 'empty bulk rids = []')

  const got = await db.bulkInsert('conf_bulk', [{ idx: 0 }, { idx: 1 }, { idx: 2 }])
  assertEqual(got.rids.length, 3, 'rids length matches input')
  assertEqual(new Set(got.rids.map(String)).size, 3, 'rids are distinct')
})

await test('generic.delete', async (db) => {
  const ins = await db.documents.insert('conf_del', { k: 'v' })
  const r = await db.documents.delete('conf_del', ins.rid)
  assertEqual(r.affected, 1, 'delete affected = 1')
  assertEqual(r.deleted, true, 'delete deleted = true')
})

// --- documents.* ------------------------------------------------------

await test('documents.crud_nested_patch', async (db) => {
  const ins = await db.documents.insert('conf_doc', {
    event_type: 'login',
    attempts: 2,
    success: true,
  })
  assert(ins.rid != null && String(ins.rid).length > 0, 'rid present')

  const got = await db.documents.get('conf_doc', ins.rid)
  assertEqual(got.event_type, 'login', 'get preserves field')

  const list = await db.documents.list('conf_doc', { limit: 10 })
  assert(Array.isArray(list.items) && list.items.length > 0, 'list returns items')

  const patched = await db.documents.patch('conf_doc', ins.rid, { attempts: 3 })
  // Spec §4.4: top-level merge MUST preserve unrelated fields.
  assertEqual(patched.event_type, 'login', 'patch preserves unrelated fields')

  const del = await db.documents.delete('conf_doc', ins.rid)
  assertEqual(del.affected, 1, 'delete affected = 1')
  assertEqual(del.deleted, true, 'delete deleted = true')
})

await test('documents.delete_missing_no_error', async (db) => {
  const ins = await db.documents.insert('conf_doc_miss', { k: 'v' })
  await db.documents.delete('conf_doc_miss', ins.rid)
  const r = await db.documents.delete('conf_doc_miss', 'rid_that_does_not_exist')
  assertEqual(r.affected, 0, 'missing delete affected = 0')
  assertEqual(r.deleted, false, 'missing delete deleted = false')
})

await test('documents.patch_empty_rejects', async (db) => {
  const ins = await db.documents.insert('conf_doc_pe', { k: 'v' })
  await expectCode(
    () => db.documents.patch('conf_doc_pe', ins.rid, {}),
    'INVALID_ARGUMENT',
    'empty patch',
  )
})

// --- kv.* -------------------------------------------------------------

await test('kv.exact_key_round_trip', async (db) => {
  await db.query('CREATE KV conf_kv')
  const kv = db.kv('conf_kv')
  await kv.set('characters:hansel', 'witch')
  assertEqual(await kv.get('characters:hansel'), 'witch', 'exact key survives set/get')
  const list = await kv.list({ prefix: 'characters:' })
  assert(list.items.some((row) => row.key === 'characters:hansel'), 'exact key in list')
})

await test('kv.missing_get_returns_none', async (db) => {
  await db.query('CREATE KV conf_kv_miss')
  const kv = db.kv('conf_kv_miss')
  await kv.set('seed', 'v')
  assertEqual(await kv.get('never:set'), null, 'missing get returns null, not NOT_FOUND')
})

await test('kv.delete_returns_envelope', async (db) => {
  await db.query('CREATE KV conf_kv_del')
  const kv = db.kv('conf_kv_del')
  await kv.set('k', 'v')
  const first = await kv.delete('k')
  assertEqual(first.affected, 1, 'first delete affected = 1')
  assertEqual(first.deleted, true, 'first delete deleted = true')
  const second = await kv.delete('k')
  assertEqual(second.affected, 0, 'second delete affected = 0')
  assertEqual(second.deleted, false, 'second delete deleted = false')
})

// --- queues.* ---------------------------------------------------------

await test('queues.fifo_peek_pop_len', async (db) => {
  await db.queues.create('conf_q_fifo')
  await db.queues.push('conf_q_fifo', { n: 1 })
  await db.queues.push('conf_q_fifo', { n: 2 })
  assertEqual(await db.queues.len('conf_q_fifo'), 2, 'len after push')
  const peeked = await db.queues.peek('conf_q_fifo', 1)
  assertEqual(peeked.length, 1, 'peek returns one')
  // Peek MUST NOT decrement length.
  assertEqual(await db.queues.len('conf_q_fifo'), 2, 'len unchanged after peek')
  const popped = await db.queues.pop('conf_q_fifo', 1)
  assertEqual(popped.length, 1, 'pop returns one')
  assertEqual(await db.queues.len('conf_q_fifo'), 1, 'len decremented after pop')
})

await test('queues.empty_pop_returns_empty', async (db) => {
  await db.queues.create('conf_q_empty')
  const out = await db.queues.pop('conf_q_empty')
  assert(Array.isArray(out) && out.length === 0, 'empty pop returns [], not error')
})

await test('queues.purge_resets_len', async (db) => {
  await db.queues.create('conf_q_purge')
  for (let i = 0; i < 3; i++) await db.queues.push('conf_q_purge', { i })
  assertEqual(await db.queues.len('conf_q_purge'), 3, 'len before purge')
  await db.queues.purge('conf_q_purge')
  assertEqual(await db.queues.len('conf_q_purge'), 0, 'len after purge')
})

// --- tx.* -------------------------------------------------------------

await test('tx.commit_persists', async (db) => {
  await db.query('CREATE TABLE conf_tx_commit (name TEXT)')
  const tx = db.tx()
  await tx.begin()
  await db.query("INSERT INTO conf_tx_commit (name) VALUES ('keep')")
  await tx.commit()
  const r = await db.query("SELECT name FROM conf_tx_commit WHERE name = 'keep'")
  assert((r.rows ?? []).some((row) => row.name === 'keep'), 'committed row visible')
})

await test('tx.rollback_discards', async (db) => {
  await db.query('CREATE TABLE conf_tx_rb (name TEXT)')
  const tx = db.tx()
  await tx.begin()
  await db.query("INSERT INTO conf_tx_rb (name) VALUES ('drop')")
  await tx.rollback()
  const r = await db.query("SELECT name FROM conf_tx_rb WHERE name = 'drop'")
  assert(!(r.rows ?? []).some((row) => row.name === 'drop'), 'rolled-back row gone')
})

// --- errors.* ---------------------------------------------------------

await test('errors.invalid_argument.empty_sql', async (db) => {
  await expectCode(() => db.query(''), 'INVALID_ARGUMENT', 'empty SQL')
})

await test('errors.not_found.document_get', async (db) => {
  const ins = await db.documents.insert('conf_err_nf', { k: 'v' })
  await db.documents.delete('conf_err_nf', ins.rid)
  await expectCode(
    () => db.documents.get('conf_err_nf', 'rid_definitely_missing'),
    'NOT_FOUND',
    'missing document get',
  )
})

// --- wire.* (provisional namespaces — SQL only in v1.0) --------------
//
// As in the Rust/Go/Dart reference harnesses, only the probabilistic HLL
// round trip is asserted here. The vectors / graph / timeseries wire surfaces
// are reachable via `db.query` (documented in the README matrix) but their SQL
// grammar is still stabilising, so they are not pinned as conformance cases in
// v1.0; helper APIs for all four namespaces land in v1.1.

await test('wire.probabilistic.hll_round_trip', async (db) => {
  await db.query('CREATE HLL conf_hll')
  await db.query("HLL ADD conf_hll 'alice' 'bob' 'alice'")
  const r = await db.query('HLL COUNT conf_hll')
  const row = r.rows?.[0] ?? {}
  // Spec accepts either `count` or `cardinality` as the projected column.
  assert('count' in row || 'cardinality' in row, `expected count/cardinality column in ${JSON.stringify(row)}`)
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed ? 1 : 0)
