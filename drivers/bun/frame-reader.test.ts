import { test } from 'bun:test'
import { strict as assert } from 'node:assert'
import { connection, countReceiveCopies, frame } from './test/frame-fixture'

test('every header and UTF-8 payload split preserves query results', async () => {
  const { db } = connection()
  const text = 'Bun 🦊 café'
  const bytes = frame(text)
  const padded = Buffer.concat([Buffer.alloc(3), bytes, Buffer.alloc(3)]).subarray(3, -3)
  for (let split = 1; split < bytes.length; split++) {
    const pending = db.query('SELECT probe')
    db._onData(padded.subarray(0, split))
    db._onData(padded.subarray(split))
    assert.equal(await pending, text)
  }
})

test('fragmented receive copies at most one frame plus a header', async () => {
  const { db } = connection()
  const bytes = frame(Buffer.alloc(1024 * 1024 - 16, 97))
  const pending = db.query('SELECT probe')
  const counts = countReceiveCopies(() => {
    for (let offset = 0; offset < bytes.length; offset += 1024) db._onData(bytes.subarray(offset, offset + 1024))
  })
  assert.equal((await pending).length, bytes.length - 16)
  assert.ok(counts.totalBytes <= bytes.length + 16, `copied ${counts.totalBytes} bytes for ${bytes.length} bytes`)
})

test('coalesced frames, bytewise headers and trailing data preserve FIFO and payload ownership', async () => {
  const { db } = connection()
  for (let round = 0; round < 128; round++) {
    const third = frame('third')
    const bytes = Buffer.concat([frame('first'), frame('second'), third.subarray(0, 7)])
    const pending = [db.query('one'), db.query('two'), db.query('three')]
    db._onData(bytes)
    for (let offset = 7; offset < third.length; offset++) db._onData(third.subarray(offset, offset + 1))
    bytes.fill(0)
    assert.deepEqual(await Promise.all(pending), ['first', 'second', 'third'])
  }
})

test('engine error frames reject only their own request', async () => {
  const { db } = connection()
  const error = assert.rejects(db.query('bad'), { code: 'ENGINE', message: 'query failed' })
  const good = db.query('good')
  db._onData(Buffer.concat([frame('query failed', 0x03), frame('ok')]))
  await error
  assert.equal(await good, 'ok')
  const again = db.query('again')
  db._onData(frame('still open'))
  assert.equal(await again, 'still open')
})

test('invalid lengths reject all requests and future writes', async () => {
  for (const length of [0, 15, 16 * 1024 * 1024 + 1, 0xffffffff]) {
    const { db, socket } = connection()
    const pending = [db.query('one'), db.query('two')].map(query => assert.rejects(query, { code: 'FRAME_INVALID_LENGTH' }))
    const bytes = Buffer.alloc(16)
    bytes.writeUInt32LE(length, 0)
    db._onData(bytes)
    await Promise.all(pending)
    const written = socket.writes.length
    await assert.rejects(db.query('later'), { code: 'FRAME_INVALID_LENGTH' })
    assert.equal(socket.writes.length, written)
  }
})

test('partial close and socket errors reject pending and future queries', async () => {
  for (const length of [0, 1, 15, 16, 19]) {
    const { db } = connection()
    const pending = [db.query('one'), db.query('two')].map(query => assert.rejects(query, { code: 'CONNECTION_CLOSED' }))
    db._onData(frame('body').subarray(0, length))
    db._onClose()
    await Promise.all(pending)
    await assert.rejects(db.query('later'), { code: 'CONNECTION_CLOSED' })
  }
  const { db } = connection()
  const failure = new Error('socket failed')
  const pending = assert.rejects(db.query('probe'), failure)
  db._onError(failure)
  await pending
  await assert.rejects(db.query('later'), failure)
})

test('local close is idempotent and rejects pending queries', async () => {
  const { db, socket } = connection()
  const pending = assert.rejects(db.query('probe'), { code: 'CONNECTION_CLOSED' })
  db.close()
  await pending
  const written = socket.writes.length
  db.close()
  assert.equal(socket.writes.length, written)
  assert.equal(socket.ended, 1)
  await assert.rejects(db.query('later'), { code: 'CONNECTION_CLOSED' })
})

test('synchronous write failure terminates all queued requests', async () => {
  const { db, socket } = connection()
  const failure = new Error('write failed')
  const first = assert.rejects(db.query('one'), failure)
  socket.write = () => { throw failure }
  await assert.rejects(db.query('two'), failure)
  await first
  await assert.rejects(db.query('later'), failure)
})

test('consumed buffers and empty views can be collected while the connection stays alive', () => {
  const result = Bun.spawnSync([process.execPath, `${import.meta.dir}/test/receive-gc.ts`], { stdout: 'pipe', stderr: 'pipe' })
  assert.equal(result.exitCode, 0, result.stderr.toString())
})
