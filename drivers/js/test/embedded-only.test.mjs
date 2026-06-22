import { test } from 'node:test'
import assert from 'node:assert/strict'

import { connect, EMBEDDED_ONLY_MESSAGE, RedDBError, uriToArgs } from '../src/index.js'

const REMOTE_URIS = [
  'http://127.0.0.1:5000',
  'https://reddb.example.com',
  'red://127.0.0.1:5050',
  'reds://reddb.example.com:5050',
  'grpc://127.0.0.1:55055',
  'grpcs://reddb.example.com:55555',
  'red://127.0.0.1:5432?proto=pg',
]

for (const uri of REMOTE_URIS) {
  test(`connect('${uri}') rejects with EMBEDDED_ONLY`, async () => {
    await assert.rejects(
      () => connect(uri),
      (err) => {
        assert.ok(err instanceof RedDBError)
        assert.equal(err.code, 'EMBEDDED_ONLY')
        assert.equal(err.message, EMBEDDED_ONLY_MESSAGE)
        assert.match(err.message, /@reddb-io\/client/)
        return true
      },
    )
  })
}

test('uriToArgs rejects remote schemes with EMBEDDED_ONLY', () => {
  assert.throws(
    () => uriToArgs('grpc://localhost:55055'),
    (err) => err instanceof RedDBError && err.code === 'EMBEDDED_ONLY',
  )
})
