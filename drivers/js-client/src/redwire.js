/**
 * RedWire client for Node / Bun / Deno — the node-bound shim over the
 * transport-agnostic codec.
 *
 * The protocol itself (frame codec, `stream_id` multiplex, handshake,
 * `RedWireClient`) lives in `./redwire-core.js`, which imports **no**
 * `node:` built-in so the exact same code rides into the browser bundle
 * (`./redwire-ws.js`, #937, ADR 0036). This module adds only what needs
 * Node: the TCP / TLS socket openers and the `node:zlib` zstd provider,
 * then exposes the historical `connectRedwire({ host, port, ... })`
 * surface the Node entry and tools depend on.
 *
 * Auth methods this cut supports: `anonymous`, `bearer`. SCRAM / mTLS /
 * OAuth land in subsequent PRs.
 */

import {
  connectRedwireOverSocket,
  setZstdProvider,
} from './redwire-core.js'

// Re-export the codec surface so existing `./redwire.js` importers
// (index.js, the redwire-result test, JSDoc type refs) keep resolving.
export {
  MessageKind,
  Features,
  ValueTag,
  BinaryTag,
  Flags,
  RedWireClient,
  decodeResultPayload,
  encodeQueryWithParams,
  encodeValue,
  connectRedwireOverSocket,
  setZstdProvider,
} from './redwire-core.js'

// Resolve the native zstd binding once per process and install it into
// the codec. Node 22+ ships `zstdCompressSync`; Deno / older Node lack it
// and the codec stays plaintext (it still honours the COMPRESSED flag bit
// peers offer). Cached so repeated connects don't re-import.
let _zstdLoaded = false
async function ensureZstd() {
  if (_zstdLoaded) return
  _zstdLoaded = true
  try {
    const zlib = await import('node:zlib')
    if (typeof zlib.zstdCompressSync === 'function') {
      setZstdProvider(zlib)
    }
  } catch {
    // node:zlib missing — Deno (no-zstd) or restricted runtime.
  }
}

/**
 * Open a RedWire connection over a TCP / TLS / Unix socket.
 *
 * @param {object} opts
 * @param {string} opts.host
 * @param {number} opts.port
 * @param {{ kind: 'anonymous' } | { kind: 'bearer', token: string }} [opts.auth]
 * @param {string} [opts.clientName]
 * @param {object} [opts.socket] Pre-connected duplex to use as-is. When
 *   set, `host`/`port`/`tls` are ignored — the byte transport is supplied
 *   by the caller (the browser WS transport rides this seam).
 * @param {object} [opts.tls] When set, wraps the socket in TLS.
 * @param {string|Buffer} [opts.tls.ca] Trusted CA bundle (PEM).
 * @param {string|Buffer} [opts.tls.cert] Client cert for mTLS (PEM).
 * @param {string|Buffer} [opts.tls.key] Client key for mTLS (PEM).
 * @param {boolean} [opts.tls.rejectUnauthorized=true] Verify server cert.
 * @param {string} [opts.tls.servername] SNI override.
 * @returns {Promise<import('./redwire-core.js').RedWireClient>}
 */
export async function connectRedwire(opts) {
  await ensureZstd()

  let socket = opts.socket
  if (!socket) {
    const { host, port } = opts
    if (typeof host !== 'string' || host.length === 0) {
      throw new TypeError('connectRedwire: host required')
    }
    if (typeof port !== 'number' || port <= 0 || port > 0xffff) {
      throw new TypeError('connectRedwire: port required (1-65535)')
    }
    socket = opts.tls
      ? await openTlsSocket(host, port, opts.tls)
      : await openSocket(host, port)
  }

  return await connectRedwireOverSocket(socket, {
    auth: opts.auth,
    clientName: opts.clientName,
  })
}

// ---------------------------------------------------------------------------
// Socket helpers — node:net / node:tls work on Node, Bun, and Deno via shim
// ---------------------------------------------------------------------------

async function openSocket(host, port) {
  const { Socket } = await import('node:net')
  return await new Promise((resolve, reject) => {
    const sock = new Socket()
    const onErr = (err) => {
      sock.removeListener('connect', onOk)
      reject(err)
    }
    const onOk = () => {
      sock.removeListener('error', onErr)
      sock.setNoDelay(true)
      resolve(sock)
    }
    sock.once('error', onErr)
    sock.once('connect', onOk)
    sock.connect(port, host)
  })
}

async function openTlsSocket(host, port, tlsOpts) {
  const tls = await import('node:tls')
  const fs = await import('node:fs/promises')
  const resolveBytes = async (input) => {
    if (input == null) return undefined
    if (typeof input === 'string' && input.includes('-----BEGIN')) return input
    if (typeof input === 'string') return await fs.readFile(input)
    return input // Buffer / Uint8Array
  }
  const ca = await resolveBytes(tlsOpts.ca)
  const cert = await resolveBytes(tlsOpts.cert)
  const key = await resolveBytes(tlsOpts.key)
  return await new Promise((resolve, reject) => {
    const sock = tls.connect({
      host,
      port,
      ca,
      cert,
      key,
      servername: tlsOpts.servername ?? host,
      rejectUnauthorized: tlsOpts.rejectUnauthorized !== false,
      ALPNProtocols: ['redwire/1'],
    })
    const onErr = (err) => {
      sock.removeListener('secureConnect', onOk)
      reject(err)
    }
    const onOk = () => {
      sock.removeListener('error', onErr)
      sock.setNoDelay(true)
      resolve(sock)
    }
    sock.once('error', onErr)
    sock.once('secureConnect', onOk)
  })
}
