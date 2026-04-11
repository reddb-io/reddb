/**
 * RedDB Wire Protocol Client for Bun
 *
 * Uses Bun's native TCP socket for maximum performance.
 * Speaks the RedDB binary TCP wire protocol directly.
 *
 * Usage:
 *   import { connect } from '@reddb/client-bun'
 *   const conn = await connect('127.0.0.1:50052')
 *   const result = await conn.query('SELECT * FROM users WHERE _entity_id = 1')
 *   conn.close()
 */

const MSG_QUERY = 0x01
const MSG_RESULT = 0x02
const MSG_ERROR = 0x03
const MSG_BULK_INSERT = 0x04
const MSG_BULK_OK = 0x05

interface PendingRequest {
  resolve: (value: { type: number; payload: Buffer }) => void
  reject: (error: Error) => void
}

export class RedDBConnection {
  private socket: ReturnType<typeof Bun.connect> extends Promise<infer T> ? T : never
  private pending: PendingRequest[] = []
  private buffer = Buffer.alloc(0)

  constructor(socket: any) {
    this.socket = socket
  }

  _onData(chunk: Buffer) {
    this.buffer = Buffer.concat([this.buffer, chunk])
    this._tryResolve()
  }

  private _tryResolve() {
    while (this.buffer.length >= 5 && this.pending.length > 0) {
      const totalLen = this.buffer.readUInt32LE(0)
      const frameSize = 4 + totalLen
      if (this.buffer.length < frameSize) break

      const msgType = this.buffer[4]
      const payload = this.buffer.subarray(5, frameSize)
      this.buffer = this.buffer.subarray(frameSize)

      const { resolve, reject } = this.pending.shift()!

      if (msgType === MSG_ERROR) {
        reject(new Error(payload.toString('utf8')))
      } else {
        resolve({ type: msgType, payload: Buffer.from(payload) })
      }
    }
  }

  private _send(msgType: number, payload: Buffer): Promise<{ type: number; payload: Buffer }> {
    return new Promise((resolve, reject) => {
      this.pending.push({ resolve, reject })
      const header = Buffer.alloc(5)
      header.writeUInt32LE(1 + payload.length, 0)
      header[4] = msgType
      this.socket.write(Buffer.concat([header, payload]))
    })
  }

  async query(sql: string): Promise<string> {
    const resp = await this._send(MSG_QUERY, Buffer.from(sql, 'utf8'))
    return resp.payload.toString('utf8')
  }

  async queryParsed(sql: string): Promise<any> {
    return JSON.parse(await this.query(sql))
  }

  async queryRaw(sql: string): Promise<number> {
    const resp = await this._send(MSG_QUERY, Buffer.from(sql, 'utf8'))
    return resp.payload.length
  }

  async bulkInsert(collection: string, jsonPayloads: string[]): Promise<number> {
    const collBuf = Buffer.from(collection, 'utf8')
    const header = Buffer.alloc(2 + collBuf.length + 4)
    header.writeUInt16LE(collBuf.length, 0)
    collBuf.copy(header, 2)
    header.writeUInt32LE(jsonPayloads.length, 2 + collBuf.length)

    const parts: Buffer[] = [header]
    for (const p of jsonPayloads) {
      const jsonBuf = Buffer.from(p, 'utf8')
      const lenBuf = Buffer.alloc(4)
      lenBuf.writeUInt32LE(jsonBuf.length, 0)
      parts.push(lenBuf, jsonBuf)
    }

    const resp = await this._send(MSG_BULK_INSERT, Buffer.concat(parts))
    if (resp.payload.length >= 8) {
      return Number(resp.payload.readBigUInt64LE(0))
    }
    return 0
  }

  close() {
    this.socket.end()
  }
}

export async function connect(addr: string): Promise<RedDBConnection> {
  const [host, portStr] = addr.split(':')
  const port = parseInt(portStr, 10)

  let connRef: RedDBConnection | null = null

  const socket = await Bun.connect({
    hostname: host,
    port,
    socket: {
      data(socket, data) {
        connRef?._onData(Buffer.from(data))
      },
      error(socket, error) {
        console.error('RedDB wire error:', error)
      },
      close() {},
      open() {},
    },
  })

  connRef = new RedDBConnection(socket)
  return connRef
}

/**
 * Connect via TLS-encrypted wire protocol.
 * @param addr - Host:port (e.g. "127.0.0.1:50053")
 * @param opts - TLS options
 */
export async function connectTls(
  addr: string,
  opts: { ca?: string; rejectUnauthorized?: boolean } = {},
): Promise<RedDBConnection> {
  const [host, portStr] = addr.split(':')
  const port = parseInt(portStr, 10)

  let connRef: RedDBConnection | null = null

  const socket = await Bun.connect({
    hostname: host,
    port,
    tls: {
      ca: opts.ca,
      rejectUnauthorized: opts.rejectUnauthorized ?? true,
    },
    socket: {
      data(socket, data) {
        connRef?._onData(Buffer.from(data))
      },
      error(socket, error) {
        console.error('RedDB wire+tls error:', error)
      },
      close() {},
      open() {},
    },
  })

  connRef = new RedDBConnection(socket)
  return connRef
}
