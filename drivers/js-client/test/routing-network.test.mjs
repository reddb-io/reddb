import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer as httpServer } from 'node:http'
import { createServer as grpcServer } from 'node:http2'
import { createServer as tcpServer } from 'node:net'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { connect } from '../src/index.js'
import { connect as browserConnect } from '../src/index.browser.js'
import { encodeFrame, decodeFrame, MessageKind as K } from '../src/redwire-core.js'
import { REDWIRE_WS_SUBPROTOCOL } from '../src/redwire-ws.js'

function integer(value, size) {
  const bytes = Buffer.alloc(size)
  if (size === 8) bytes.writeBigUInt64LE(BigInt(value))
  else bytes.writeUInt32LE(value)
  return bytes
}
const string = (value) => Buffer.concat([integer(Buffer.byteLength(value), 4), Buffer.from(value)])
function topologyBytes(topology) {
  const body = Buffer.concat([
    integer(topology.epoch, 8), string(topology.primary.addr), string(''), integer(topology.replicas.length, 4),
    ...topology.replicas.flatMap((replica) => [string(replica.addr), string(''), Buffer.from([1]), integer(0, 4), integer(100, 8)]),
    Buffer.alloc(topology.replicas.length),
  ])
  return Buffer.concat([Buffer.from([1]), integer(body.length, 4), body])
}
function varint(value) {
  const bytes = []
  do { bytes.push((value & 127) | (value > 127 ? 128 : 0)); value >>>= 7 } while (value)
  return Buffer.from(bytes)
}
const protobufBytes = (field, bytes) => Buffer.concat([varint(field * 8 + 2), varint(bytes.length), bytes])
function grpcMessage(bytes) {
  const header = Buffer.alloc(5)
  header.writeUInt32BE(bytes.length, 1)
  return Buffer.concat([header, bytes])
}
const result = (node) => ({ statement: 'select', columns: ['node'], records: [{ node }], affected: 0 })

function wireSession(node, membership, calls, send) {
  let buffered = Buffer.alloc(0)
  let preface = false
  return (chunk) => {
    buffered = Buffer.concat([buffered, chunk])
    if (!preface) {
      if (buffered.length < 2) return
      buffered = buffered.subarray(2)
      preface = true
    }
    while (buffered.length >= 16) {
      const length = buffered.readUInt32LE(0)
      if (buffered.length < length) return
      const frame = decodeFrame(buffered.subarray(0, length))
      buffered = buffered.subarray(length)
      let kind = K.Result
      let payload
      if (frame.kind === K.Hello) {
        kind = K.HelloAck
        payload = { auth: 'bearer', features: 3 }
      } else if (frame.kind === K.AuthResponse) {
        assert.equal(JSON.parse(Buffer.from(frame.payload).toString()).token, 'test-token')
        kind = K.AuthOk
        payload = { session_id: 'test', features: 3 }
      } else if (frame.kind === K.Topology) {
        payload = topologyBytes(membership())
      } else if (frame.kind === K.QueryWithParams) {
        const bytes = Buffer.from(frame.payload)
        calls.push([node, bytes.subarray(4, 4 + bytes.readUInt32LE(0)).toString()])
        payload = result(node)
      } else if (frame.kind === K.BulkInsert) {
        calls.push([node, 'insert'])
        kind = K.BulkOk
        payload = { affected: 1, rid: '1', id: '1', rids: ['1'], ids: ['1'] }
      } else if (frame.kind === K.Bye) {
        kind = K.Bye
        payload = Buffer.alloc(0)
      } else {
        assert.fail(`unexpected wire request ${frame.kind}`)
      }
      const bytes = payload instanceof Uint8Array ? payload : Buffer.from(JSON.stringify(payload))
      send(encodeFrame(kind, frame.correlationId, bytes))
    }
  }
}

function wsPacket(bytes, opcode = 2) {
  const header = Buffer.alloc(bytes.length < 126 ? 2 : 4)
  header[0] = 0x80 | opcode
  header[1] = bytes.length < 126 ? bytes.length : 126
  if (bytes.length >= 126) header.writeUInt16BE(bytes.length, 2)
  return Buffer.concat([header, bytes])
}

async function startNode(protocol, node, membership, calls) {
  let server
  if (protocol === 'grpc') {
    server = grpcServer()
    server.on('stream', (stream, headers) => {
      assert.equal(headers.authorization, 'Bearer test-token')
      const chunks = []
      stream.on('data', (bytes) => chunks.push(bytes))
      stream.on('end', () => {
        let reply
        if (headers[':path'].endsWith('/Topology')) {
          assert.equal(headers['x-reddb-topology-transport'], 'grpc')
          reply = protobufBytes(1, topologyBytes(membership()))
        } else {
          const body = Buffer.concat(chunks).subarray(5)
          const sql = body.subarray(2, 2 + body[1]).toString()
          calls.push([node, sql])
          reply = Buffer.concat([Buffer.from([8, 1]), protobufBytes(3, Buffer.from('select')),
            protobufBytes(7, Buffer.from(JSON.stringify(result(node))))])
        }
        stream.respond({ ':status': 200, 'content-type': 'application/grpc', 'grpc-status': '0' })
        stream.end(grpcMessage(reply))
      })
    })
  } else if (protocol === 'http' || protocol === 'ws') {
    server = httpServer(async (request, response) => {
      assert.ok(request.url.startsWith('/cluster-a/'), request.url)
      const path = request.url.slice('/cluster-a'.length)
      assert.equal(request.headers.authorization, 'Bearer test-token')
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      const value = path.startsWith('/v1/topology') ? membership() : result(node)
      if (path === '/query') calls.push([node, JSON.parse(Buffer.concat(chunks)).query])
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify(value))
    })
    if (protocol === 'ws') server.on('upgrade', (request, socket) => {
      assert.equal(request.url, '/cluster-a/socket')
      const accept = createHash('sha1').update(request.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')
      socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nSec-WebSocket-Protocol: ${REDWIRE_WS_SUBPROTOCOL}\r\n\r\n`)
      const feed = wireSession(node, membership, calls, (bytes) => socket.write(wsPacket(Buffer.from(bytes))))
      let buffered = Buffer.alloc(0)
      socket.on('data', (bytes) => {
        buffered = Buffer.concat([buffered, bytes])
        while (buffered.length >= 6) {
          let length = buffered[1] & 127
          const offset = length === 126 ? 4 : 2
          if (length === 126) length = buffered.readUInt16BE(2)
          if (buffered.length < offset + 4 + length) return
          const opcode = buffered[0] & 15
          const mask = buffered.subarray(offset, offset + 4)
          const payload = Buffer.from(buffered.subarray(offset + 4, offset + 4 + length))
          for (let index = 0; index < payload.length; index++) payload[index] ^= mask[index % 4]
          buffered = buffered.subarray(offset + 4 + length)
          if (opcode === 8) { socket.end(); return }
          if (opcode === 2) feed(payload)
        }
      })
    })
  } else {
    server = tcpServer((socket) => socket.on('data', wireSession(node, membership, calls, (bytes) => socket.write(bytes))))
  }
  const sockets = new Set()
  server.on('connection', (socket) => {
    sockets.add(socket)
    socket.on('error', () => {})
    socket.on('close', () => sockets.delete(socket))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  return {
    address: `${protocol}://127.0.0.1:${server.address().port}${protocol === 'http' ? '/cluster-a' : protocol === 'ws' ? '/cluster-a/socket' : ''}`,
    async close() {
      for (const socket of sockets) socket.destroy()
      await new Promise((resolve) => server.close(resolve))
    },
  }
}

for (const [protocol, factory, runtime] of [
  ['http', connect, 'Node'], ['grpc', connect, 'Node'], ['red', connect, 'Node'], ['ws', connect, 'Node'],
  ['http', browserConnect, 'browser'], ['ws', browserConnect, 'browser'],
]) {
  test(`${runtime}/${protocol}: real connections discover and route using one seed`, { timeout: 10_000 }, async () => {
    const calls = []
    let topology
    const primary = await startNode(protocol, 'primary', () => topology, calls)
    const replica = await startNode(protocol, 'replica', () => topology, calls)
    topology = { version: 1, epoch: '9007199254740993', primary: { addr: primary.address },
      replicas: [{ addr: replica.address, healthy: true, lag_ms: 0, last_applied_lsn: '100', rebootstrapping: false }] }
    let db
    try {
      db = await factory(primary.address, { auth: { token: 'test-token' } })
      assert.equal((await db.query('SELECT node FROM users')).rows[0].node, 'primary')
      assert.equal((await db.queryWithOptions('SELECT node FROM users', { consistency: 'local' })).rows[0].node, 'replica')
      await db.query('UPDATE users SET node = $1', 'updated')
      assert.equal(calls.at(-1)[0], 'primary')
      await db.transaction(async (transaction) => {
        assert.equal((await transaction.query('SELECT node FROM users')).rows[0].node, 'primary')
        topology = { ...topology, primary: { addr: replica.address }, replicas: [] }
        await db.client.refresh()
        assert.equal((await transaction.query('SELECT node FROM users')).rows[0].node, 'primary')
      })
      db.client.refreshAt = 0
      await db.query('UPDATE users SET node = $1', 'promoted')
      assert.equal(calls.at(-1)[0], 'replica')
      assert.equal((await db.query('SELECT node FROM users')).rows[0].node, 'replica')
    } finally {
      await db?.close()
      await primary.close()
      await replica.close()
    }
  })
}
