/**
 * Unit tests for the embedded-URI rejection helper.
 *
 * Mirrors the Rust `red_client` binary's `is_embedded_uri` so the
 * thin JS client and the thin Rust binary reject the same URIs with
 * the same wording.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import {
  EMBEDDED_REJECTION_MESSAGE,
  EmbeddedNotSupported,
  isEmbeddedUri,
  rejectEmbeddedUri,
} from '../src/embedded-rejection.js'
import { connect, RedDBError } from '../src/index.js'

// -----------------------------------------------------------------
// Embedded URIs that MUST be rejected.
// -----------------------------------------------------------------

const EMBEDDED_URIS = [
  'memory://',
  'memory:',
  'file:///tmp/foo.rdb',
  'file:///abs/path/data.rdb',
  'red:///',
  'red:///abs/path.rdb',
  'red://:memory',
  'red://:memory:',
  'red://',
  'red:',
]

for (const uri of EMBEDDED_URIS) {
  test(`isEmbeddedUri('${uri}') === true`, () => {
    assert.equal(isEmbeddedUri(uri), true)
  })

  test(`rejectEmbeddedUri('${uri}') throws EmbeddedNotSupported`, () => {
    assert.throws(
      () => rejectEmbeddedUri(uri),
      (err) => {
        assert.ok(err instanceof EmbeddedNotSupported, 'is EmbeddedNotSupported')
        assert.equal(err.code, 'EmbeddedNotSupported')
        assert.equal(err.message, EMBEDDED_REJECTION_MESSAGE)
        assert.equal(err.uri, uri)
        assert.ok(err instanceof RedDBError, 'is also RedDBError')
        return true
      },
    )
  })

  test(`connect('${uri}') rejects with EmbeddedNotSupported`, async () => {
    await assert.rejects(
      () => connect(uri),
      (err) => {
        assert.ok(err instanceof EmbeddedNotSupported)
        assert.equal(err.code, 'EmbeddedNotSupported')
        assert.match(err.message, /Use the full `red` binary/)
        return true
      },
    )
  })
}

// -----------------------------------------------------------------
// Remote URIs that MUST NOT be rejected by the parser layer.
// (`connect()` may still fail to dial, but the URI itself parses.)
// -----------------------------------------------------------------

const REMOTE_URIS = [
  'red://localhost:5050',
  'reds://reddb.example.com:5050',
  'grpc://host:5055',
  'grpcs://host:5056',
  'http://localhost:8080',
  'https://reddb.example.com',
  'red://user:pass@host:5050',
  'grpc://host:5055?token=sk-abc',
]

for (const uri of REMOTE_URIS) {
  test(`isEmbeddedUri('${uri}') === false`, () => {
    assert.equal(isEmbeddedUri(uri), false)
  })

  test(`rejectEmbeddedUri('${uri}') passes through`, () => {
    assert.equal(rejectEmbeddedUri(uri), uri.trim())
  })
}

// -----------------------------------------------------------------
// Wording invariant: must include the exact hint from red_client.rs.
// -----------------------------------------------------------------

test('EMBEDDED_REJECTION_MESSAGE matches the Rust binary wording', () => {
  assert.match(EMBEDDED_REJECTION_MESSAGE, /embedded schemes/)
  assert.match(
    EMBEDDED_REJECTION_MESSAGE,
    /Use the full `red` binary for in-memory or file-backed engines\./,
  )
})

// -----------------------------------------------------------------
// Type guards.
// -----------------------------------------------------------------

test('connect() with empty/non-string URI throws TypeError', async () => {
  await assert.rejects(() => connect(''), TypeError)
  await assert.rejects(() => connect(undefined), TypeError)
  await assert.rejects(() => connect(42), TypeError)
})
