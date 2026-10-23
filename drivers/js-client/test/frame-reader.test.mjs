import test from 'node:test'
import assert from 'node:assert/strict'
import * as core from '../src/redwire-core.js'
import { countFrameCopies, openFrameReader } from './fixtures/frame-reader.mjs'

const frame = (value, size = 32) => core.encodeFrame(
  core.MessageKind.Result, BigInt(value), new Uint8Array(size).fill(value), 0, value,
)

test('frame reader preserves every header/payload split and nonzero view offsets', async () => {
  const { socket, reader } = await openFrameReader(core)
  const bytes = frame(7)
  const padded = new Uint8Array(bytes.length + 6)
  padded.set(bytes, 3)
  const input = padded.subarray(3, -3)
  for (let split = 1; split < bytes.length; split++) {
    const pending = reader.next()
    socket.emit('data', input.subarray(0, split))
    socket.emit('data', input.subarray(split))
    assert.deepEqual(await pending, core.decodeFrame(bytes))
  }
})

test('length and payload decode across single-byte chunks', async () => {
  const { socket, reader } = await openFrameReader(core)
  for (let value = 1; value <= 3; value++) {
    const bytes = frame(value)
    const pending = reader.next()
    for (let offset = 0; offset < bytes.length; offset++) {
      socket.emit('data', bytes.subarray(offset, offset + 1))
    }
    assert.deepEqual(await pending, core.decodeFrame(bytes))
  }
})

test('fragmented frame accumulation copies at most one frame plus its header', async () => {
  const { socket, reader } = await openFrameReader(core)
  const bytes = frame(1, 1024 * 1024 - 16)
  const pending = reader.next()
  const copiedBytes = countFrameCopies(() => {
    for (let offset = 0; offset < bytes.length; offset += 1024) {
      socket.emit('data', bytes.subarray(offset, offset + 1024))
    }
  })
  assert.deepEqual(await pending, core.decodeFrame(bytes))
  assert.ok(copiedBytes <= bytes.length + 16, `copied ${copiedBytes} bytes for ${bytes.length} bytes`)
})

test('coalesced Buffer frames retain payload ownership and an incomplete trailing frame', async () => {
  const { socket, reader } = await openFrameReader(core)
  const first = frame(1)
  const second = frame(2, 55)
  const third = frame(3)
  const combined = Buffer.concat([first, second, third.subarray(0, 11)])
  const pending = [reader.next(), reader.next()]
  socket.emit('data', combined)
  const received = await Promise.all(pending)
  assert.deepEqual(received, [core.decodeFrame(first), core.decodeFrame(second)])
  const tail = reader.next()
  socket.emit('data', third.subarray(11))
  assert.deepEqual(await tail, core.decodeFrame(third))
  combined.fill(0)
  assert.deepEqual(received, [core.decodeFrame(first), core.decodeFrame(second)])
})

test('buffered frames drain in order when their consumer is paused', async () => {
  const { socket, reader } = await openFrameReader(core)
  for (let i = 0; i < 2048; i++) socket.emit('data', frame(i % 200))
  socket.emit('end')
  for (let i = 0; i < 2048; i++) {
    assert.deepEqual(await reader.next(), core.decodeFrame(frame(i % 200)))
  }
  await assert.rejects(reader.next(), { code: 'CONNECTION_CLOSED' })
})

test('invalid lengths and flags reject all waiting reads and future reads', { timeout: 5000 }, async () => {
  for (const length of [0, 15, 16 * 1024 * 1024 + 1, 0xffffffff, null]) {
    const { socket, reader } = await openFrameReader(core)
    const bytes = frame(1)
    if (length === null) bytes[5] = 0x80
    else new DataView(bytes.buffer).setUint32(0, length, true)
    const code = length === null ? 'FRAME_UNKNOWN_FLAGS' : 'FRAME_INVALID_LENGTH'
    const reads = [reader.next(), reader.next()].map(pending => assert.rejects(pending, { code }))
    socket.emit('data', bytes)
    await Promise.all(reads)
    await assert.rejects(reader.next(), { code })
  }
})

test('EOF rejects partial headers and partial payloads without leaving a read pending', { timeout: 5000 }, async () => {
  for (const event of ['end', 'close']) {
    for (const length of [1, 15, 16, 47]) {
      const { socket, reader } = await openFrameReader(core)
      const pending = assert.rejects(reader.next(), { code: 'CONNECTION_CLOSED' })
      socket.emit('data', frame(1).subarray(0, length))
      socket.emit(event)
      await pending
      await assert.rejects(reader.next(), { code: 'CONNECTION_CLOSED' })
    }
  }
})

test('socket errors reject pending and future reads', { timeout: 5000 }, async () => {
  const { socket, reader } = await openFrameReader(core)
  const error = new Error('probe socket failed')
  const reads = [reader.next(), reader.next()].map(pending => assert.rejects(pending, error))
  socket.emit('data', frame(1).subarray(0, 20))
  socket.emit('error', error)
  await Promise.all(reads)
  await assert.rejects(reader.next(), error)
})
