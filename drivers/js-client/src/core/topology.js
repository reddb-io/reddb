import { RedDBError } from './errors.js'

const MAX_REPLICAS = 256
const decoder = new TextDecoder('utf-8', { fatal: true })

/** Canonical v1 topology shared by gRPC and RedWire, including WS. */
export function decodeTopology(bytes) {
  if (bytes.length < 5) throw new RedDBError('PROTOCOL', 'truncated topology')
  if (bytes[0] !== 1) return null
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const end = 5 + view.getUint32(1, true)
  if (end !== bytes.length) throw new RedDBError('PROTOCOL', 'invalid topology length')
  let offset = 5
  function take(size) {
    if (size > end - offset) throw new RedDBError('PROTOCOL', 'truncated topology field')
    const start = offset
    offset += size
    return start
  }
  const uint32 = () => view.getUint32(take(4), true)
  const uint64 = () => view.getBigUint64(take(8), true).toString()
  function string() {
    const size = uint32()
    if (size > 4096) throw new RedDBError('PROTOCOL', 'topology address too long')
    const start = take(size)
    return decoder.decode(bytes.subarray(start, start + size))
  }
  const epoch = uint64()
  const primary = { addr: string(), region: string() }
  const count = uint32()
  if (count > MAX_REPLICAS) throw new RedDBError('PROTOCOL', 'too many topology replicas')
  const replicas = []
  for (let index = 0; index < count; index++) {
    replicas.push({
      addr: string(), region: string(), healthy: bytes[take(1)] === 1,
      lag_ms: uint32(), last_applied_lsn: uint64(), rebootstrapping: false,
    })
  }
  if (end - offset >= count) {
    for (const replica of replicas) replica.rebootstrapping = bytes[take(1)] !== 0
  }
  return { version: 1, epoch, primary, replicas }
}

export function topologyAddress(address, protocol) {
  if (typeof address !== 'string' || address.length === 0 || address.length > 4096) return null
  try {
    const uri = new URL(address.includes('://') ? address : `${protocol}://${address}`)
    if (uri.protocol !== `${protocol}:` || !uri.hostname || uri.username || uri.password
      || uri.search || uri.hash) return null
    return uri.href.replace(/\/$/, '')
  } catch {
    return null
  }
}

/** Only a single, conservatively classified SELECT may leave the writer.
 * Unknown functions, CTEs, locking reads and session statements stay there.
 */
function sqlTokens(sql) {
  if (typeof sql !== 'string' || sql.length > 1_048_576) return null
  const tokens = []
  let index = 0
  while (index < sql.length) {
    const tail = sql.slice(index)
    const whitespace = /^\s+/.exec(tail)
    if (whitespace) { index += whitespace[0].length; continue }
    if (tail.startsWith('--')) {
      const end = sql.indexOf('\n', index + 2)
      index = end < 0 ? sql.length : end + 1
      continue
    }
    if (tail.startsWith('/*')) {
      let depth = 1
      index += 2
      while (index < sql.length && depth > 0) {
        if (sql.startsWith('/*', index)) { depth++; index += 2 }
        else if (sql.startsWith('*/', index)) { depth--; index += 2 }
        else index++
        if (depth > 64) return null
      }
      if (depth !== 0) return null
      continue
    }
    const char = sql[index]
    if (char === "'" || char === '"' || char === '`') {
      const quote = char
      index++
      let closed = false
      while (index < sql.length) {
        if (sql[index] === '\\') return null
        if (sql[index++] === quote) {
          if (sql[index] === quote) { index++; continue }
          closed = true
          break
        }
      }
      if (!closed) return null
      tokens.push(quote === "'" ? '#value' : '#identifier')
      continue
    }
    const word = /^[A-Za-z_][A-Za-z_0-9]*/.exec(tail)
    if (word) { tokens.push(word[0].toUpperCase()); index += word[0].length }
    else { tokens.push(char); index++ }
  }
  if (tokens.at(-1) === ';') tokens.pop()
  return tokens
}

export function isReplicaQuery(sql) {
  const tokens = sqlTokens(sql)
  if (!tokens || tokens[0] !== 'SELECT') return false
  const forbidden = new Set([';', 'INSERT', 'UPDATE', 'DELETE', 'CREATE', 'ALTER', 'DROP',
    'INTO', 'FOR', 'LOCK', 'CALL', 'EXECUTE', 'SET', 'QUEUE', 'COPY'])
  const functions = new Set(['COUNT', 'SUM', 'MIN', 'MAX', 'AVG', 'LOWER', 'UPPER',
    'LENGTH', 'COALESCE', 'ABS', 'ROUND', 'CAST', 'IN', 'EXISTS'])
  for (let token = 0; token < tokens.length; token++) {
    if (forbidden.has(tokens[token])) return false
    if (tokens[token + 1] === '(' && /^(?:[A-Z_]|#identifier)/.test(tokens[token])
      && !functions.has(tokens[token]) && !['SELECT', 'WHERE', 'AND', 'OR', 'NOT', 'FROM'].includes(tokens[token])) {
      return false
    }
  }
  return true
}

export function transactionCommand(sql) {
  const tokens = sqlTokens(sql)
  if (!tokens?.length) return null
  if (tokens[0] === 'BEGIN') return 'BEGIN'
  if (tokens[0] === 'START' && tokens[1] === 'TRANSACTION') return 'START TRANSACTION'
  if (tokens.includes(';')) return null
  if (tokens[0] === 'COMMIT') return 'COMMIT'
  if (tokens[0] === 'ROLLBACK' && !tokens.includes('TO')) return 'ROLLBACK'
  return null
}
