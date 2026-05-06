/**
 * Tests for @reddb-io/internal-asset-fetcher.
 *
 * Run: node test/fetch.test.mjs
 */

import { createServer } from 'node:http'

import { fetchReleaseAsset } from '../src/index.js'
import { composeAssetName, UnsupportedPlatformError } from '../src/asset-name.js'
import { verifySha256, sha256Hex, ChecksumMismatchError } from '../src/checksum.js'
import {
  downloadFollowingRedirects,
  AssetNotFoundError,
  HttpError,
  TooManyRedirectsError,
} from '../src/download.js'

let passed = 0
let failed = 0

async function test(name, fn) {
  try {
    await fn()
    console.log(`  ok  ${name}`)
    passed++
  } catch (err) {
    console.error(`  FAIL ${name}\n        ${err.stack || err.message}`)
    failed++
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(`assertion failed: ${msg}`)
}

function assertEqual(actual, expected, msg) {
  if (actual !== expected) {
    throw new Error(`${msg}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`)
  }
}

async function assertRejects(promise, predicate, msg) {
  try {
    await promise
  } catch (err) {
    if (!predicate(err)) {
      throw new Error(`${msg}: error did not match predicate (got: ${err.code || ''} ${err.message})`)
    }
    return
  }
  throw new Error(`${msg}: expected reject`)
}

function assertThrows(fn, predicate, msg) {
  try {
    fn()
  } catch (err) {
    if (!predicate(err)) {
      throw new Error(`${msg}: error did not match predicate (got: ${err.message})`)
    }
    return
  }
  throw new Error(`${msg}: expected throw`)
}

function startServer(handler) {
  return new Promise((resolve) => {
    const server = createServer(handler)
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address()
      resolve({ server, port, url: (path) => `http://127.0.0.1:${port}${path}` })
    })
  })
}

function stopServer(server) {
  return new Promise((resolve) => server.close(() => resolve()))
}

console.log('@reddb-io/internal-asset-fetcher tests')

// -----------------------------------------------------------------------
// composeAssetName — platform/arch mapping
// -----------------------------------------------------------------------

await test('composeAssetName covers every shipped platform/arch', () => {
  const cases = [
    [{ platform: 'linux', arch: 'x64', binName: 'red' }, 'red-linux-x86_64'],
    [{ platform: 'linux', arch: 'arm64', binName: 'red' }, 'red-linux-aarch64'],
    [{ platform: 'linux', arch: 'arm', binName: 'red' }, 'red-linux-armv7'],
    [{ platform: 'linux', arch: 'armv7l', binName: 'red' }, 'red-linux-armv7'],
    [{ platform: 'darwin', arch: 'x64', binName: 'red' }, 'red-macos-x86_64'],
    [{ platform: 'darwin', arch: 'arm64', binName: 'red' }, 'red-macos-aarch64'],
    [{ platform: 'win32', arch: 'x64', binName: 'red' }, 'red-windows-x86_64.exe'],
  ]
  for (const [input, expected] of cases) {
    assertEqual(composeAssetName(input), expected, `${input.platform}/${input.arch}`)
  }
})

await test('composeAssetName honours alternative binName (e.g. red_client)', () => {
  assertEqual(
    composeAssetName({ platform: 'linux', arch: 'arm64', binName: 'red_client' }),
    'red_client-linux-aarch64',
    'red_client linux arm64',
  )
  assertEqual(
    composeAssetName({ platform: 'win32', arch: 'x64', binName: 'red_client' }),
    'red_client-windows-x86_64.exe',
    'red_client windows x64',
  )
})

await test('composeAssetName throws UnsupportedPlatformError on unknown combo', () => {
  assertThrows(
    () => composeAssetName({ platform: 'freebsd', arch: 'x64', binName: 'red' }),
    (err) =>
      err instanceof UnsupportedPlatformError &&
      err.code === 'UNSUPPORTED_PLATFORM' &&
      /freebsd/.test(err.message) &&
      /x64/.test(err.message),
    'freebsd/x64 rejected',
  )
  assertThrows(
    () => composeAssetName({ platform: 'win32', arch: 'arm64', binName: 'red' }),
    (err) => err.code === 'UNSUPPORTED_PLATFORM',
    'win32/arm64 rejected (no asset shipped today)',
  )
})

await test('composeAssetName rejects empty binName', () => {
  assertThrows(
    () => composeAssetName({ platform: 'linux', arch: 'x64', binName: '' }),
    (err) => err instanceof TypeError && /binName/.test(err.message),
    'empty binName',
  )
})

// -----------------------------------------------------------------------
// verifySha256
// -----------------------------------------------------------------------

await test('verifySha256 accepts matching digest', () => {
  const buf = Buffer.from('hello reddb', 'utf8')
  verifySha256(buf, sha256Hex(buf))
})

await test('verifySha256 throws ChecksumMismatchError on mismatch', () => {
  const buf = Buffer.from('hello reddb', 'utf8')
  assertThrows(
    () => verifySha256(buf, '0000000000000000000000000000000000000000000000000000000000000000'),
    (err) =>
      err instanceof ChecksumMismatchError &&
      err.code === 'CHECKSUM_MISMATCH' &&
      err.actual === sha256Hex(buf) &&
      err.expected === '0000000000000000000000000000000000000000000000000000000000000000',
    'sha256 mismatch reported',
  )
})

await test('verifySha256 normalises case + whitespace on expected', () => {
  const buf = Buffer.from('hello reddb', 'utf8')
  const hex = sha256Hex(buf)
  verifySha256(buf, `  ${hex.toUpperCase()}  `)
})

// -----------------------------------------------------------------------
// downloadFollowingRedirects — body, redirects, 404, too-many-redirects
// -----------------------------------------------------------------------

await test('downloadFollowingRedirects returns 200 body as Buffer', async () => {
  const payload = Buffer.from('binary-bytes-here')
  const { server, url } = await startServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/octet-stream' })
    res.end(payload)
  })
  try {
    const got = await downloadFollowingRedirects(url('/asset'))
    assert(Buffer.isBuffer(got), 'returns Buffer')
    assert(got.equals(payload), 'body bytes match')
  } finally {
    await stopServer(server)
  }
})

await test('downloadFollowingRedirects follows a 3-hop redirect chain', async () => {
  const payload = Buffer.from('final-asset-bytes')
  let hits = 0
  const { server, url } = await startServer((req, res) => {
    hits++
    if (req.url === '/start') {
      res.writeHead(302, { Location: url('/mid') })
      res.end()
      return
    }
    if (req.url === '/mid') {
      res.writeHead(301, { Location: '/final' }) // relative location
      res.end()
      return
    }
    if (req.url === '/final') {
      res.writeHead(200)
      res.end(payload)
      return
    }
    res.writeHead(500)
    res.end()
  })
  try {
    const got = await downloadFollowingRedirects(url('/start'))
    assert(got.equals(payload), 'final body matches')
    assertEqual(hits, 3, 'exactly 3 hops')
  } finally {
    await stopServer(server)
  }
})

await test('downloadFollowingRedirects raises AssetNotFoundError on 404', async () => {
  const startUrl = (port) => `http://127.0.0.1:${port}/missing`
  const { server, port } = await startServer((req, res) => {
    res.writeHead(404)
    res.end('not here')
  })
  try {
    await assertRejects(
      downloadFollowingRedirects(startUrl(port)),
      (err) => err instanceof AssetNotFoundError && err.code === 'ASSET_NOT_FOUND',
      '404 maps to AssetNotFoundError',
    )
  } finally {
    await stopServer(server)
  }
})

await test('downloadFollowingRedirects raises HttpError on non-404 non-2xx', async () => {
  const { server, url } = await startServer((req, res) => {
    res.writeHead(503)
    res.end('busy')
  })
  try {
    await assertRejects(
      downloadFollowingRedirects(url('/x')),
      (err) => err instanceof HttpError && err.code === 'HTTP_ERROR' && err.status === 503,
      '503 maps to HttpError',
    )
  } finally {
    await stopServer(server)
  }
})

await test('downloadFollowingRedirects raises TooManyRedirectsError after 5 hops', async () => {
  const { server, url } = await startServer((req, res) => {
    const n = Number(req.url.replace('/r/', ''))
    res.writeHead(302, { Location: `/r/${n + 1}` })
    res.end()
  })
  try {
    await assertRejects(
      downloadFollowingRedirects(url('/r/0')),
      (err) => err instanceof TooManyRedirectsError && err.code === 'TOO_MANY_REDIRECTS',
      'redirect cycle is bounded',
    )
  } finally {
    await stopServer(server)
  }
})

// -----------------------------------------------------------------------
// fetchReleaseAsset — input validation
// -----------------------------------------------------------------------

await test('fetchReleaseAsset validates required fields', async () => {
  await assertRejects(
    fetchReleaseAsset({ tag: 'v1', platform: 'linux', arch: 'x64', binName: 'red' }),
    (err) => err instanceof TypeError && /repo/.test(err.message),
    'missing repo',
  )
  await assertRejects(
    fetchReleaseAsset({ repo: 'a/b', platform: 'linux', arch: 'x64', binName: 'red' }),
    (err) => err instanceof TypeError && /tag/.test(err.message),
    'missing tag',
  )
  await assertRejects(
    fetchReleaseAsset({ repo: 'a/b', tag: 'v1', arch: 'x64', binName: 'red' }),
    (err) => err instanceof TypeError && /platform/.test(err.message),
    'missing platform',
  )
  await assertRejects(
    fetchReleaseAsset({ repo: 'a/b', tag: 'v1', platform: 'linux', binName: 'red' }),
    (err) => err instanceof TypeError && /arch/.test(err.message),
    'missing arch',
  )
})

await test('fetchReleaseAsset surfaces UNSUPPORTED_PLATFORM before any network call', async () => {
  // No HTTP server stood up — if the function reached the download stage,
  // this would hang or fail with ECONNREFUSED, not UNSUPPORTED_PLATFORM.
  await assertRejects(
    fetchReleaseAsset({
      repo: 'reddb-io/reddb',
      tag: 'v0.0.0',
      platform: 'plan9',
      arch: 'mips',
      binName: 'red',
    }),
    (err) => err.code === 'UNSUPPORTED_PLATFORM',
    'platform check is the first failure mode',
  )
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed > 0 ? 1 : 0)
