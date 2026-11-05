import { test } from 'bun:test'
import { strict as assert } from 'node:assert'
import { createServer, type Socket } from 'node:net'
import { connect } from './index'
import { frame } from './test/frame-fixture'

async function peer(immediateClose = false) {
  const sockets = new Set<Socket>()
  const server = createServer(socket => {
    sockets.add(socket)
    socket.on('close', () => sockets.delete(socket))
    socket.on('error', () => {}) // The client deliberately closes malformed peers.
    if (immediateClose) { socket.end(); return }
    let input = Buffer.alloc(0)
    let preamble = false
    socket.on('data', chunk => {
      input = Buffer.concat([input, chunk])
      if (!preamble) {
        if (input.length < 2) return
        assert.deepEqual([...input.subarray(0, 2)], [0xfe, 1])
        input = input.subarray(2)
        preamble = true
      }
      while (input.length >= 16) {
        const length = input.readUInt32LE(0)
        assert(length >= 16 && length <= 16 * 1024 * 1024)
        if (input.length < length) return
        const request = input.subarray(0, length)
        input = input.subarray(length)
        if (request[4] === 0x16) { socket.end(); return }
        const sql = request.subarray(16).toString()
        let response: Buffer
        if (request[4] === 0x10) response = frame('{"auth":"anonymous","features":0}', 0x11)
        else if (request[4] === 0x13) response = frame('{"features":0}', 0x14)
        else if (sql === 'peer-close') { socket.end(); return }
        else if (sql === 'malformed') { socket.end(Buffer.alloc(16)); return }
        else if (sql === 'partial') { socket.end(frame('unfinished body').subarray(0, 19)); return }
        else response = frame(JSON.stringify({ data: 'r'.repeat(512 * 1024) }))
        request.copy(response, 8, 8, 16)
        for (let offset = 0; offset < response.length; offset += 1024) socket.write(response.subarray(offset, offset + 1024))
      }
    })
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  assert(address && typeof address === 'object')
  return {
    endpoint: `127.0.0.1:${address.port}`,
    async close() {
      for (const socket of sockets) socket.destroy()
      await new Promise<void>(resolve => server.close(() => resolve()))
    },
  }
}

test('actual Bun TCP preserves a large query result', async () => {
  const server = await peer()
  let db: Awaited<ReturnType<typeof connect>> | undefined
  try {
    db = await connect(server.endpoint)
    assert.deepEqual(await db.queryParsed('large'), { data: 'r'.repeat(512 * 1024) })
  } finally { db?.close(); await server.close() }
})

for (const sql of ['peer-close', 'partial', 'malformed']) {
  test(`actual Bun TCP rejects ${sql} and future queries`, async () => {
    const server = await peer()
    let db: Awaited<ReturnType<typeof connect>> | undefined
    try {
      db = await connect(server.endpoint)
      const code = sql === 'malformed' ? 'FRAME_INVALID_LENGTH' : 'CONNECTION_CLOSED'
      await assert.rejects(db.query(sql), { code })
      await assert.rejects(db.query('later'), { code })
    } finally { db?.close(); await server.close() }
  })
}

test('actual Bun TCP rejects a peer that closes before authentication', async () => {
  const server = await peer(true)
  try { await assert.rejects(connect(server.endpoint)) }
  finally { await server.close() }
})
