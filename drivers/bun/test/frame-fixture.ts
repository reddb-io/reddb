import { RedDBConnection } from '../index'

export function connection(Constructor = RedDBConnection) {
  const socket = {
    writes: [] as Buffer[],
    ended: 0,
    write(bytes: Buffer) { this.writes.push(bytes); return bytes.length },
    end() { this.ended++ },
  }
  return { socket, db: new Constructor(socket) }
}

export function frame(payload: string | Buffer, kind = 0x02): Buffer {
  const body = typeof payload === 'string' ? Buffer.from(payload) : payload
  const bytes = Buffer.alloc(16 + body.length)
  bytes.writeUInt32LE(bytes.length, 0)
  bytes[4] = kind
  bytes.writeBigUInt64LE(1n, 8)
  body.copy(bytes, 16)
  return bytes
}

// Count top-level receive copies; suppress nested built-in calls to avoid double counting.
export function countReceiveCopies(run: () => void) {
  const concat = Buffer.concat
  const from = Buffer.from
  const copy = Buffer.prototype.copy
  let depth = 0
  const counts = { concatBytes: 0, ownedBytes: 0 }
  Buffer.concat = function (chunks, ...args) {
    if (depth === 0) counts.concatBytes += chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0)
    depth++
    try { return Reflect.apply(concat, Buffer, [chunks, ...args]) } finally { depth-- }
  } as typeof Buffer.concat
  Buffer.from = function (source: any, ...args: any[]) {
    if (depth === 0 && ArrayBuffer.isView(source)) counts.ownedBytes += source.byteLength
    depth++
    try { return Reflect.apply(from, Buffer, [source, ...args]) } finally { depth-- }
  } as typeof Buffer.from
  Buffer.prototype.copy = function (...args) {
    const copied = Reflect.apply(copy, this, args)
    if (depth === 0) counts.ownedBytes += copied
    return copied
  }
  try { run() } finally {
    Buffer.concat = concat
    Buffer.from = from
    Buffer.prototype.copy = copy
  }
  return { ...counts, totalBytes: counts.concatBytes + counts.ownedBytes }
}
