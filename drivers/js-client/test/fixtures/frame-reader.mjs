// Scripted anonymous handshake; the production reader handles every data event.
export async function openFrameReader(core) {
  const listeners = new Map()
  const socket = {
    on(event, listener) {
      if (!listeners.has(event)) listeners.set(event, [])
      listeners.get(event).push(listener)
    },
    emit(event, value) {
      for (const listener of listeners.get(event) ?? []) listener(value)
    },
    write(bytes, done) {
      if (bytes.length > 2) {
        const frame = core.decodeFrame(bytes)
        const kind = frame.kind === core.MessageKind.Hello
          ? core.MessageKind.HelloAck : core.MessageKind.AuthOk
        const payload = new TextEncoder().encode(JSON.stringify({ auth: 'anonymous', features: 0 }))
        const reply = core.encodeFrame(kind, frame.correlationId, payload)
        queueMicrotask(() => socket.emit('data', reply))
      }
      done()
    },
    end() { socket.emit('end') },
  }
  const client = await core.connectRedwireOverSocket(socket)
  return { socket, reader: client.reader }
}

// Count accumulator writes, separately from timings and the decoder's payload slice.
export function countFrameCopies(run) {
  const original = Uint8Array.prototype.set
  let copiedBytes = 0
  Uint8Array.prototype.set = function (source, offset) {
    copiedBytes += source.byteLength
    return original.call(this, source, offset)
  }
  try { run() } finally { Uint8Array.prototype.set = original }
  return copiedBytes
}
