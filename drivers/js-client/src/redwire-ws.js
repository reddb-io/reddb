/**
 * RedWire-over-binary-WebSocket transport for the browser (#937, ADR 0036).
 *
 * The browser cannot open a raw TCP socket, so it cannot speak
 * RedWire-over-TCP — but it can open a binary `WebSocket`. This module
 * adapts a binary WebSocket to the node-socket-shaped duplex the
 * transport-agnostic codec (`./redwire-core.js`) consumes, then runs the
 * exact same handshake + `stream_id` multiplex over it. The browser thus
 * speaks the **same** multiplexed binary protocol as the native drivers.
 *
 * Imports only `./redwire-core.js` and `./protocol.js` — both free of any
 * `node:` built-in — so this stays inside the browser bundle graph.
 */

import { RedDBError } from './protocol.js'
import { connectRedwireOverSocket } from './redwire-core.js'

/** Server route the upgrade lands on (must match `ws_edge.rs`). */
export const REDWIRE_WS_PATH = '/redwire'

/** WebSocket subprotocol advertised on the upgrade (must match the server). */
export const REDWIRE_WS_SUBPROTOCOL = 'reddb.redwire.v1'

/** Coerce a WebSocket `message` payload into a `Uint8Array`, or null. */
function toBytes(data) {
  if (data instanceof ArrayBuffer) return new Uint8Array(data)
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
  }
  // Text frames are not RedWire bytes — ignore.
  return null
}

/**
 * Adapt a binary `WebSocket` to the node-socket-shaped duplex the RedWire
 * codec expects: `.on('data'|'error'|'end'|'close', cb)`, `.write(bytes,
 * cb)`, `.end()`. `FrameReader` reassembles frames from the `data`
 * events; message boundaries need not align with frame boundaries since
 * RedWire framing is self-delimiting.
 */
export class WebSocketDuplex {
  constructor(ws) {
    this.ws = ws
    this._handlers = { data: [], error: [], end: [], close: [] }
    this._closed = false

    if ('binaryType' in ws) ws.binaryType = 'arraybuffer'

    ws.addEventListener('message', (ev) => {
      const bytes = toBytes(ev.data)
      if (bytes) this._emit('data', bytes)
    })
    ws.addEventListener('error', () => {
      this._emit('error', new RedDBError('WS_TRANSPORT', 'redwire websocket transport error'))
    })
    ws.addEventListener('close', () => {
      if (this._closed) return
      this._closed = true
      this._emit('close')
    })
  }

  on(event, cb) {
    if (this._handlers[event]) this._handlers[event].push(cb)
    return this
  }

  _emit(event, arg) {
    for (const cb of this._handlers[event] ?? []) cb(arg)
  }

  write(bytes, cb) {
    try {
      // Browser `WebSocket.send` accepts an ArrayBufferView and ships
      // exactly the view's bytes as one binary message. There is no
      // write-callback in the WS API, so resolve synchronously — the
      // browser owns its own send buffer / backpressure.
      this.ws.send(bytes)
      if (cb) cb()
    } catch (err) {
      if (cb) cb(err)
      else this._emit('error', err)
    }
    return true
  }

  end() {
    this._closed = true
    try {
      this.ws.close()
    } catch {
      // already closing/closed
    }
  }
}

/** Resolve once the socket is OPEN; reject on early error/close. */
function waitOpen(ws) {
  return new Promise((resolve, reject) => {
    if (ws.readyState === 1 /* OPEN */) {
      resolve()
      return
    }
    const cleanup = () => {
      ws.removeEventListener('open', onOpen)
      ws.removeEventListener('error', onErr)
      ws.removeEventListener('close', onClose)
    }
    const onOpen = () => {
      cleanup()
      resolve()
    }
    const onErr = () => {
      cleanup()
      reject(new RedDBError('WS_CONNECT_FAILED', 'redwire websocket failed to open'))
    }
    const onClose = () => {
      cleanup()
      reject(new RedDBError('WS_CONNECT_FAILED', 'redwire websocket closed before opening'))
    }
    ws.addEventListener('open', onOpen)
    ws.addEventListener('error', onErr)
    ws.addEventListener('close', onClose)
  })
}

/**
 * Open a RedWire connection over a binary WebSocket.
 *
 * @param {object} opts
 * @param {string} [opts.url] `wss://host:port/redwire` endpoint.
 * @param {{ kind: 'anonymous' } | { kind: 'bearer', token: string }} [opts.auth]
 * @param {string} [opts.clientName]
 * @param {string} [opts.subprotocol] Override the advertised subprotocol.
 * @param {Function} [opts.WebSocketImpl] WebSocket constructor (defaults
 *   to `globalThis.WebSocket`); also the test seam for a mock.
 * @param {object} [opts.socket] Pre-built duplex to use as-is, bypassing
 *   the real WS open (test seam).
 * @returns {Promise<import('./redwire-core.js').RedWireClient>}
 */
export async function connectRedwireWs(opts = {}) {
  const { url, auth, clientName, subprotocol = REDWIRE_WS_SUBPROTOCOL } = opts

  // Test seam: a caller-supplied duplex skips the live WebSocket open.
  if (opts.socket) {
    return await connectRedwireOverSocket(opts.socket, { auth, clientName })
  }

  const WS = opts.WebSocketImpl ?? globalThis.WebSocket
  if (typeof WS !== 'function') {
    throw new RedDBError(
      'NO_WEBSOCKET',
      'no global WebSocket in this runtime; pass opts.WebSocketImpl',
    )
  }
  if (typeof url !== 'string' || !url.startsWith('wss://')) {
    throw new RedDBError(
      'WSS_REQUIRED',
      `redwire websocket requires a wss:// url, got '${url}'`,
    )
  }

  const ws = new WS(url, subprotocol)
  if ('binaryType' in ws) ws.binaryType = 'arraybuffer'
  await waitOpen(ws)

  const duplex = new WebSocketDuplex(ws)
  return await connectRedwireOverSocket(duplex, { auth, clientName })
}
