import { createHash } from 'node:crypto'

export class ChecksumMismatchError extends Error {
  constructor(expected, actual) {
    super(`checksum mismatch: expected sha256=${expected}, got sha256=${actual}`)
    this.name = 'ChecksumMismatchError'
    this.code = 'CHECKSUM_MISMATCH'
    this.expected = expected
    this.actual = actual
  }
}

export function sha256Hex(buf) {
  return createHash('sha256').update(buf).digest('hex')
}

export function verifySha256(buf, expected) {
  const expectedNorm = String(expected).trim().toLowerCase()
  const actual = sha256Hex(buf)
  if (actual !== expectedNorm) {
    throw new ChecksumMismatchError(expectedNorm, actual)
  }
}
