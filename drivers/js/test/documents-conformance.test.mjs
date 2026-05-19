/**
 * SDK Helper Spec — documents.* conformance harness (JS driver port).
 *
 * Mirrors the Rust harness at `crates/reddb-client/tests/conformance.rs`
 * for the documents.* case IDs defined in §12 of `docs/spec/sdk-helpers.md`.
 *
 * Cases covered:
 *   - documents.crud_nested_patch
 *   - documents.delete_missing_no_error
 *   - documents.patch_empty_rejects
 *   - errors.not_found.document_get
 *
 * Runs against memory:// using the locally built `red` binary. Skips with
 * exit 0 when the binary is absent so CI on dev machines without a prior
 * `cargo build` is not blocked.
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

async function test(name, fn) {
  try {
    await fn()
    console.log(`  ok  ${name}`)
    passed++
  } catch (err) {
    console.error(`  FAIL ${name}\n        ${err.stack || err.message}`)
    failed++
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

await test('documents.crud_nested_patch', async () => {
  const db = await connect('memory://', { binary: BINARY })
  try {
    const inserted = await db.documents.insert('conf_events', {
      event_type: 'login',
      attempts: 2,
      success: true,
    })
    assert(inserted.rid != null && String(inserted.rid).length > 0, 'rid present')
    assertEqual(inserted.affected, 1, 'affected = 1')

    const fetched = await db.documents.get('conf_events', inserted.rid)
    assertEqual(fetched.rid, inserted.rid, 'get rid round trip')
    assertEqual(fetched.event_type, 'login', 'get preserves field')

    const listed = await db.documents.list('conf_events', { limit: 10 })
    assert(Array.isArray(listed.items) && listed.items.length > 0, 'list returns items')

    const patched = await db.documents.patch('conf_events', inserted.rid, { attempts: 3 })
    // Top-level merge patch: unrelated fields MUST survive.
    assertEqual(
      patched.event_type,
      'login',
      'patch preserves unrelated fields',
    )

    const del = await db.documents.delete('conf_events', inserted.rid)
    assertEqual(del.affected, 1, 'delete affected = 1')
  } finally {
    await db.close()
  }
})

await test('documents.delete_missing_no_error', async () => {
  const db = await connect('memory://', { binary: BINARY })
  try {
    // Seed the collection so the table exists.
    const ins = await db.documents.insert('conf_events_missing', { k: 'v' })
    await db.documents.delete('conf_events_missing', ins.rid)
    // Deleting an absent rid MUST NOT raise; affected = 0.
    const r = await db.documents.delete('conf_events_missing', 'rid_that_does_not_exist')
    assertEqual(r.affected, 0, 'missing delete affected = 0')
  } finally {
    await db.close()
  }
})

await test('documents.patch_empty_rejects', async () => {
  const db = await connect('memory://', { binary: BINARY })
  try {
    const ins = await db.documents.insert('conf_events_patch', { k: 'v' })
    let raised = null
    try {
      await db.documents.patch('conf_events_patch', ins.rid, {})
    } catch (err) {
      raised = err
    }
    // The spec demands INVALID_ARGUMENT on empty patches. The current JS
    // implementation degrades to a get() — this test pins the spec contract
    // and will go red the moment a stricter validation lands, which is the
    // failing-then-passing case for documents.patch.
    assert(raised != null, 'empty patch must raise')
    assert(raised instanceof RedDBError, 'expected RedDBError')
    assertEqual(raised.code, 'INVALID_ARGUMENT', 'patch empty error code')
  } finally {
    await db.close()
  }
})

await test('errors.not_found.document_get', async () => {
  const db = await connect('memory://', { binary: BINARY })
  try {
    // Seed the collection so the SELECT doesn't trip on a missing table.
    const ins = await db.documents.insert('conf_errors_nf', { k: 'v' })
    await db.documents.delete('conf_errors_nf', ins.rid)
    let raised = null
    try {
      await db.documents.get('conf_errors_nf', 'rid_definitely_missing')
    } catch (err) {
      raised = err
    }
    assert(raised != null, 'missing get must raise')
    assert(raised instanceof RedDBError, 'expected RedDBError')
    assertEqual(raised.code, 'NOT_FOUND', 'missing get error code')
  } finally {
    await db.close()
  }
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed ? 1 : 0)
