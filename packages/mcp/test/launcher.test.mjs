/**
 * Tests for @reddb-io/mcp — the npx launcher.
 *
 * Covers binary resolution (env override > local > download via the
 * reused asset-fetcher) and the `red mcp` spawn wiring.
 *
 * Run: node test/launcher.test.mjs
 */

import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { binaryName, tryResolveBinary, ensureBinary, DEFAULT_REPO } from '../src/binary.js'
import { spawnMcp, redMcpArgs } from '../src/spawn.js'

let passed = 0
let failed = 0

function test(name, fn) {
  try {
    const r = fn()
    if (r && typeof r.then === 'function') {
      return r.then(
        () => {
          console.log(`  ok  ${name}`)
          passed++
        },
        (err) => {
          console.error(`  FAIL ${name}\n        ${err.stack || err.message}`)
          failed++
        },
      )
    }
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

function withTempPkg(fn) {
  const root = mkdtempSync(join(tmpdir(), 'reddb-mcp-'))
  try {
    return fn(root)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

const SAVED_ENV = { ...process.env }
function withEnv(extra, fn) {
  for (const k of Object.keys(extra)) {
    if (extra[k] === undefined) delete process.env[k]
    else process.env[k] = extra[k]
  }
  try {
    return fn()
  } finally {
    for (const k of Object.keys(extra)) {
      if (SAVED_ENV[k] === undefined) delete process.env[k]
      else process.env[k] = SAVED_ENV[k]
    }
  }
}

console.log('@reddb-io/mcp launcher tests')

const run = async () => {
  // ---- resolve ---------------------------------------------------------

  await test('binaryName is platform-specific', () => {
    assertEqual(binaryName('linux'), 'red', 'unix → red')
    assertEqual(binaryName('darwin'), 'red', 'macos → red')
    assertEqual(binaryName('win32'), 'red.exe', 'windows → red.exe')
  })

  await test('REDDB_BIN override is resolved verbatim (no download)', () =>
    withTempPkg((root) =>
      withEnv({ REDDB_BIN: '/opt/custom/red' }, () => {
        const resolved = tryResolveBinary({ packageRoot: root, platform: 'linux' })
        assertEqual(resolved, '/opt/custom/red', 'env override returned verbatim')
      }),
    ))

  await test('local bin/red is resolved when present', () =>
    withTempPkg((root) =>
      withEnv({ REDDB_BIN: undefined }, () => {
        const binDir = join(root, 'bin')
        mkdirSync(binDir, { recursive: true })
        const local = join(binDir, 'red')
        writeFileSync(local, '')
        assertEqual(tryResolveBinary({ packageRoot: root, platform: 'linux' }), local, 'local resolved')
      }),
    ))

  await test('tryResolveBinary returns null when nothing is available', () =>
    withTempPkg((root) =>
      withEnv({ REDDB_BIN: undefined }, () => {
        assertEqual(tryResolveBinary({ packageRoot: root, platform: 'linux' }), null, 'null when absent')
      }),
    ))

  await test('ensureBinary returns the override without fetching', () =>
    withTempPkg((root) =>
      withEnv({ REDDB_BIN: '/opt/custom/red', REDDB_MCP_VERSION: undefined }, async () => {
        let fetched = false
        const path = await ensureBinary({
          version: '1.15.0',
          packageRoot: root,
          platform: 'linux',
          arch: 'x64',
          fetchAsset: async () => {
            fetched = true
            return Buffer.from('')
          },
        })
        assertEqual(path, '/opt/custom/red', 'override path returned')
        assert(!fetched, 'fetcher must not be called when override is set')
      }),
    ))

  await test('ensureBinary downloads + writes the binary when missing', () =>
    withTempPkg((root) =>
      withEnv({ REDDB_BIN: undefined, REDDB_MCP_VERSION: undefined, REDDB_MCP_REPO: undefined }, async () => {
        const calls = []
        const path = await ensureBinary({
          version: '1.15.0',
          packageRoot: root,
          platform: 'linux',
          arch: 'x64',
          fetchAsset: async (opts) => {
            calls.push(opts)
            return Buffer.from('FAKE-RED-BINARY')
          },
        })
        assertEqual(calls.length, 1, 'fetcher called exactly once')
        assertEqual(calls[0].repo, DEFAULT_REPO, 'default repo used')
        assertEqual(calls[0].tag, 'v1.15.0', 'tag tracks the package version')
        assertEqual(calls[0].platform, 'linux', 'platform forwarded')
        assertEqual(calls[0].arch, 'x64', 'arch forwarded')
        assertEqual(calls[0].binName, 'red', 'binName is red')
        const expected = join(root, 'bin', 'red')
        assertEqual(path, expected, 'returns the cached binary path')
        assert(existsSync(expected), 'binary written to disk')
        assertEqual(readFileSync(expected, 'utf8'), 'FAKE-RED-BINARY', 'downloaded body persisted')
      }),
    ))

  await test('ensureBinary honours REDDB_MCP_VERSION / REDDB_MCP_REPO overrides', () =>
    withTempPkg((root) =>
      withEnv({ REDDB_BIN: undefined, REDDB_MCP_VERSION: '2.0.0', REDDB_MCP_REPO: 'me/fork' }, async () => {
        let seen
        await ensureBinary({
          version: '1.15.0',
          packageRoot: root,
          platform: 'linux',
          arch: 'arm64',
          fetchAsset: async (opts) => {
            seen = opts
            return Buffer.from('x')
          },
        })
        assertEqual(seen.tag, 'v2.0.0', 'REDDB_MCP_VERSION wins over pkg version')
        assertEqual(seen.repo, 'me/fork', 'REDDB_MCP_REPO wins over default')
      }),
    ))

  await test('ensureBinary requires a version', () =>
    withTempPkg(async (root) => {
      let threw = false
      try {
        await ensureBinary({ packageRoot: root, fetchAsset: async () => Buffer.from('') })
      } catch (err) {
        threw = /version/.test(err.message)
      }
      assert(threw, 'missing version throws TypeError mentioning version')
    }))

  // ---- spawn -----------------------------------------------------------

  await test('spawnMcp execs `red mcp` over inherited stdio', () => {
    withEnv({ REDDB_MCP_URI: undefined }, () => {
      const calls = []
      const fakeChild = { on() {} }
      const fakeSpawn = (bin, argv, opts) => {
        calls.push({ bin, argv, opts })
        return fakeChild
      }
      const child = spawnMcp('/abs/red', [], { spawn: fakeSpawn })
      assertEqual(child, fakeChild, 'returns the child process')
      assertEqual(calls.length, 1, 'spawned once')
      assertEqual(calls[0].bin, '/abs/red', 'spawns the resolved binary')
      assertEqual(JSON.stringify(calls[0].argv), JSON.stringify(['mcp']), 'argv is [mcp]')
      assertEqual(calls[0].opts.stdio, 'inherit', 'stdio inherited')
    })
  })

  await test('spawnMcp forwards extra args after the mcp subcommand', () => {
    withEnv({ REDDB_MCP_URI: undefined }, () => {
      const calls = []
      const fakeSpawn = (bin, argv) => {
        calls.push(argv)
        return { on() {} }
      }
      spawnMcp('/abs/red', ['--url', 'tcp://127.0.0.1:6789'], { spawn: fakeSpawn })
      assertEqual(
        JSON.stringify(calls[0]),
        JSON.stringify(['mcp', '--url', 'tcp://127.0.0.1:6789']),
        'extra args forwarded after mcp',
      )
    })
  })

  await test('redMcpArgs forwards REDDB_MCP_URI as the connection URI', () =>
    withEnv({ REDDB_MCP_URI: 'file:///tmp/reddb-agent.rdb' }, () => {
      assertEqual(
        JSON.stringify(redMcpArgs([], process.env)),
        JSON.stringify(['mcp', '--uri', 'file:///tmp/reddb-agent.rdb']),
        'env URI becomes --uri',
      )
    }))

  await test('redMcpArgs lets explicit --uri beat REDDB_MCP_URI', () =>
    withEnv({ REDDB_MCP_URI: 'red://env.example:5050' }, () => {
      assertEqual(
        JSON.stringify(redMcpArgs(['--uri', 'memory://'], process.env)),
        JSON.stringify(['mcp', '--uri', 'memory://']),
        'explicit URI is preserved',
      )
    }))

  await test('spawnMcp rejects an empty binary path', () => {
    let threw = false
    try {
      spawnMcp('', [], { spawn: () => ({ on() {} }) })
    } catch (err) {
      threw = /binary/.test(err.message)
    }
    assert(threw, 'empty binary throws')
  })

  console.log(`\n${passed} passed, ${failed} failed`)
  process.exit(failed > 0 ? 1 : 0)
}

run()
