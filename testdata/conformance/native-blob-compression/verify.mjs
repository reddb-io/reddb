// Independent reader for the reddb-file fixture. Requires Node with native Zstd.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import * as zlib from 'node:zlib';

assert.equal(typeof zlib.zstdDecompressSync, 'function', 'Node native Zstd is required');
assert.equal(typeof zlib.crc32, 'function', 'Node native CRC32 is required');
const fixture = JSON.parse(readFileSync(new URL('./manifest.json', import.meta.url)));
const page = Buffer.from(fixture.encodedPageHex, 'hex');
const format = fixture.format;
assert.equal(page.subarray(0, 4).toString('ascii'), format.magic);
assert.equal(page.readUInt32LE(format.nextPageOffset), fixture.nextPage);
const chunkBytes = page.readUInt32LE(format.chunkLengthOffset);
assert.equal(page.length, format.headerBytes + chunkBytes);
const decodedBytes = page.readBigUInt64LE(format.decodedLengthOffset);
assert.equal(decodedBytes, BigInt(fixture.decodedBytes));
function decode(limitBytes) {
  assert.ok(decodedBytes > 0n && decodedBytes <= BigInt(limitBytes), 'decoded budget');
  const decoded = zlib.zstdDecompressSync(page.subarray(format.headerBytes), {
    maxOutputLength: limitBytes,
  });
  assert.equal(BigInt(decoded.length), decodedBytes);
  assert.equal(zlib.crc32(decoded), page.readUInt32LE(format.checksumOffset));
  return decoded;
}
const decoded = decode(fixture.decodedLimitBytes);
assert.equal(zlib.crc32(decoded), fixture.decodedCrc32);
assert.equal(createHash('sha256').update(decoded).digest('hex'), fixture.decodedSha256);
assert.equal(decoded.subarray(0, 4).toString('ascii'), fixture.logicalFrame.magic);
assert.throws(() => decode(fixture.decodedBytes - 1), /decoded budget/);
console.log(JSON.stringify({
  reader: 'node:zlib', node: process.version,
  storedBytes: chunkBytes, decodedBytes: decoded.length,
  sha256: fixture.decodedSha256, budgetRejection: true,
}));
