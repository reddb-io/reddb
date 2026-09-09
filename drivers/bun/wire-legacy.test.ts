import { strict as assert } from 'node:assert'
import { createServer } from 'node:net'
import { connect, RedDBError } from './index'

// The old protocol is intentionally summary-only. A small protocol peer lets
// us exercise capability negotiation without installing an obsolete engine.
const queries: number[] = []
const server = createServer((socket) => {
  socket.setTimeout(5_000, () => socket.destroy())
  let input = Buffer.alloc(0)
  let magicReceived = false
  socket.on('data', (chunk) => {
    input = Buffer.concat([input, chunk])
    if (!magicReceived) {
      if (input.length < 2) return
      assert.deepEqual([...input.subarray(0, 2)], [0xfe, 1])
      input = input.subarray(2)
      magicReceived = true
    }
    while (input.length >= 16) {
      const length = input.readUInt32LE(0)
      assert(length >= 16 && length <= 16 * 1024 * 1024)
      if (input.length < length) return
      const frame = input.subarray(0, length)
      input = input.subarray(length)
      const kind = frame[4]
      if (kind === 0x16) { socket.end(); return }
      let responseKind: number
      let body: object
      if (kind === 0x10) {
        responseKind = 0x11
        body = { auth: 'anonymous', features: 0 }
      } else if (kind === 0x13) {
        responseKind = 0x14
        body = { features: 0 }
      } else {
        queries.push(kind)
        responseKind = 0x02
        body = { ok: true, affected: 0, statement: 'select' }
      }
      const payload = Buffer.from(JSON.stringify(body))
      const response = Buffer.alloc(16 + payload.length)
      response.writeUInt32LE(response.length, 0)
      response[4] = responseKind
      frame.copy(response, 8, 8, 16)
      payload.copy(response, 16)
      socket.write(response)
    }
  })
})
await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
const address = server.address()
assert(address && typeof address === 'object')
let db: Awaited<ReturnType<typeof connect>> | undefined
try {
  db = await connect(`127.0.0.1:${address.port}`)
  assert.equal(db.supportsParams(), false)
  assert.deepEqual(await db.queryParsed('SELECT 42'), { ok: true, affected: 0, statement: 'select' })
  await assert.rejects(db.query('SELECT $1', [42]),
    (error) => error instanceof RedDBError && error.code === 'PARAMS_UNSUPPORTED')
  assert.deepEqual(queries, [0x01])
  console.log('ok Bun legacy capability fallback')
} finally {
  db?.close()
  await new Promise<void>((resolve) => server.close(() => resolve()))
}
