// Integration test for the in-process addon. Needs a built `reddb.node`:
//   cd drivers/node && cargo build && cp <target>/libreddb_node.so /path/reddb.node
//   REDDB_NATIVE_ADDON=/path/reddb.node node --test test/native-engine.e2e.test.mjs
// Skips (does not fail) when no addon is available.
import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'

import { connect, RedDBError } from '../src/index.js'
import { resolveNativeAddon } from '../src/native-engine.js'

const addon = resolveNativeAddon()
const opts = { skip: addon ? false : 'no reddb.node (set REDDB_NATIVE_ADDON)' }

const rows = (r) => (r.rows ?? r).map((x) => ({ ...x }))

test('memory:// round-trips through the addon, no subprocess', opts, async () => {
  const db = await connect('memory://')
  try {
    await db.query('CREATE TABLE t (id INTEGER, name TEXT)')
    await db.query("INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b')")
    const r = await db.query('SELECT id, name FROM t ORDER BY id')
    assert.deepEqual(rows(r).map((x) => [x.id, x.name]), [[1, 'a'], [2, 'b']])
  } finally {
    await db.close()
  }
})

test('a failing statement rejects with a RedDBError and the engine stays usable', opts, async () => {
  const db = await connect('memory://')
  try {
    await assert.rejects(db.query('SELEKT nonsense'), (e) => e instanceof RedDBError && typeof e.code === 'string')
    const r = await db.query('SELECT 1 AS one')
    assert.equal(rows(r)[0].one, 1)
  } finally {
    await db.close()
  }
})

test('transactions commit on success and roll back on error', opts, async () => {
  const db = await connect('memory://')
  try {
    await db.query('CREATE TABLE tx (id INTEGER)')
    await db.transaction(async (tx) => {
      await tx.query('INSERT INTO tx (id) VALUES (1)')
    })
    await assert.rejects(
      db.transaction(async (tx) => {
        await tx.query('INSERT INTO tx (id) VALUES (2)')
        throw new Error('boom')
      }),
      /boom/,
    )
    const r = await db.query('SELECT id FROM tx ORDER BY id')
    assert.deepEqual(rows(r).map((x) => x.id), [1])
  } finally {
    await db.close()
  }
})

test('un-awaited requests are applied in call order', opts, async () => {
  const db = await connect('memory://')
  try {
    await db.query('CREATE TABLE ord (n INTEGER)')
    const pending = []
    for (let n = 1; n <= 25; n++) pending.push(db.query(`INSERT INTO ord (n) VALUES (${n})`))
    pending.push(db.query('SELECT n FROM ord ORDER BY n'))
    const results = await Promise.all(pending)
    const last = rows(results.at(-1)).map((x) => x.n)
    assert.deepEqual(last, Array.from({ length: 25 }, (_, i) => i + 1), 'the select saw every earlier insert')
  } finally {
    await db.close()
  }
})

test('file:// persists across close/open and holds the single-writer lock', opts, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'reddb-native-'))
  const file = path.join(dir, 'data.rdb')
  try {
    const first = await connect(`file://${file}`)
    await first.query('CREATE TABLE p (id INTEGER, v TEXT)')
    await first.query("INSERT INTO p (id, v) VALUES (7, 'persisted')")
    await assert.rejects(connect(`file://${file}`), /open|lock|writer/i, 'second writer must be refused')
    await first.close()
    await first.close() // idempotent

    const second = await connect(`file://${file}`)
    const r = await second.query('SELECT v FROM p WHERE id = 7')
    assert.equal(rows(r)[0].v, 'persisted')
    await second.close()
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
})

test('calls after close reject with CLIENT_CLOSED', opts, async () => {
  const db = await connect('memory://')
  await db.close()
  await assert.rejects(db.query('SELECT 1'), (e) => e.code === 'CLIENT_CLOSED')
})
