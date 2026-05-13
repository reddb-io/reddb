import { test } from 'node:test'
import assert from 'node:assert/strict'

import { connect, EMBEDDED_ONLY_MESSAGE, RedDBError, uriToArgs } from '../src/index.js'

const REMOTE_URIS = [
  'http://127.0.0.1:8080',
  'https://reddb.example.com',
  'red://127.0.0.1:5050',
  'reds://reddb.example.com:5050',
  'grpc://127.0.0.1:5055',
  'grpcs://reddb.example.com:5056',
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
    () => uriToArgs('grpc://localhost:5055'),
    (err) => err instanceof RedDBError && err.code === 'EMBEDDED_ONLY',
  )
})
