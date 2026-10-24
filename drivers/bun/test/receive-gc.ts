import { strict as assert } from 'node:assert'
import { connection, frame } from './frame-fixture'

const { db } = connection()
function receive() {
  const bytes = frame(Buffer.alloc(1024 * 1024 - 16, 97))
  const weak = [new WeakRef(bytes.buffer)]
  const concat = Buffer.concat
  Buffer.concat = function (...args) {
    const result = Reflect.apply(concat, Buffer, args)
    weak.push(new WeakRef(result.buffer))
    return result
  }
  try {
    for (let offset = 0; offset < bytes.length; offset += 1024) db._onData(bytes.subarray(offset, offset + 1024))
  } finally { Buffer.concat = concat }
  return weak
}
const pending = db.query('SELECT probe')
const weak = receive()
const value = await pending
async function assertCollected(references: WeakRef<ArrayBufferLike>[], message: string) {
  // Collection timing varies with VM scheduling; a live connection must still
  // release every backing store within a bounded observation window.
  for (let i = 0; i < 100; i++) {
    await Bun.sleep(10)
    Bun.gc(true)
    if (references.every(ref => ref.deref() === undefined)) return
  }
  assert.fail(message)
}
await assertCollected(weak, 'idle connection retained consumed receive storage')
assert.equal(value.length, 1024 * 1024 - 16)

function empty() {
  const bytes = Buffer.alloc(1024 * 1024)
  db._onData(bytes.subarray(0, 0))
  return new WeakRef(bytes.buffer)
}
const emptyBuffer = empty()
await assertCollected([emptyBuffer], 'empty view retained its backing storage')
