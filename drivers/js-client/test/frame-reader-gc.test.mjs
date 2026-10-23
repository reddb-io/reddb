import test from 'node:test'
import { execFileSync } from 'node:child_process'

test('an idle reader releases consumed frame buffers and ignores empty views', () => {
  execFileSync(process.execPath, ['--expose-gc', '--input-type=module', '-e', `
    import assert from 'node:assert/strict'
    import { setImmediate as nextTurn } from 'node:timers/promises'
    import * as core from './src/redwire-core.js'
    import { openFrameReader } from './test/fixtures/frame-reader.mjs'
    const { socket, reader } = await openFrameReader(core)
    function receive() {
      const bytes = core.encodeFrame(core.MessageKind.Result, 1n, new Uint8Array(1024 * 1024 - 16).fill(7))
      const weak = [new WeakRef(bytes.buffer)]
      const original = Uint8Array.prototype.set
      Uint8Array.prototype.set = function (source, offset) {
        weak.push(new WeakRef(this.buffer))
        return original.call(this, source, offset)
      }
      try {
        for (let offset = 0; offset < bytes.length; offset += 1024) socket.emit('data', bytes.subarray(offset, offset + 1024))
      } finally { Uint8Array.prototype.set = original }
      return weak
    }
    const pending = reader.next()
    const weak = receive()
    const result = await pending
    for (let i = 0; i < 4; i++) { await nextTurn(); globalThis.gc() }
    assert.ok(weak.every(ref => ref.deref() === undefined), 'idle reader retained consumed backing storage')
    assert.equal(result.payload.length, 1024 * 1024 - 16)
    assert.equal(result.payload[0], 7)
    function empty() {
      const buffer = new ArrayBuffer(1024 * 1024)
      socket.emit('data', new Uint8Array(buffer, 0, 0))
      return new WeakRef(buffer)
    }
    const emptyBuffer = empty()
    for (let i = 0; i < 4; i++) { await nextTurn(); globalThis.gc() }
    assert.equal(emptyBuffer.deref(), undefined, 'empty view retained its backing buffer')
    assert.equal(reader.totalLen, 0)
  `], { cwd: new URL('..', import.meta.url), timeout: 10000 })
})
