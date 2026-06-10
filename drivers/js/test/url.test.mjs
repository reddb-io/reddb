/**
 * Connection-string parser tests. Guards the legacy `reds://`/`grpc(s)://`
 * branch against the userinfo regression: `reds://user:pass@host:5050` must
 * parse host/port/credentials (a naive `split(':')` made host become `user`
 * and port NaN, so the RedDB Cloud conn string never reached the server).
 */

import test from 'node:test'
import assert from 'node:assert/strict'

import { parseUri } from '../src/url.js'

test('reds:// with userinfo parses host, port and credentials', () => {
  const p = parseUri('reds://admin:s3cr%40t@db1.org1.db.reddb.io:5050')
  assert.equal(p.kind, 'reds')
  assert.equal(p.host, 'db1.org1.db.reddb.io')
  assert.equal(p.port, 5050)
  assert.equal(p.username, 'admin')
  assert.equal(p.password, 's3cr@t')
})

test('reds:// without userinfo keeps legacy behaviour (default port)', () => {
  const p = parseUri('reds://db1.org1.db.reddb.io')
  assert.equal(p.kind, 'reds')
  assert.equal(p.host, 'db1.org1.db.reddb.io')
  assert.equal(p.port, 5050)
  assert.equal(p.username, undefined)
  assert.equal(p.password, undefined)
})

test('reds:// carries token and loginUrl query params', () => {
  const p = parseUri('reds://host:5050?token=sk-abc&loginUrl=https%3A%2F%2Fapi.example%2Flogin')
  assert.equal(p.token, 'sk-abc')
  assert.equal(p.loginUrl, 'https://api.example/login')
})

test('grpc:// and grpcs:// keep legacy host/port behaviour', () => {
  const g = parseUri('grpc://localhost:5555')
  assert.equal(g.kind, 'grpc')
  assert.equal(g.host, 'localhost')
  assert.equal(g.port, 5555)

  const gs = parseUri('grpcs://remote.example')
  assert.equal(gs.kind, 'grpcs')
  assert.equal(gs.host, 'remote.example')
})

test('reds:// with empty host throws', () => {
  assert.throws(() => parseUri('reds://'))
})
