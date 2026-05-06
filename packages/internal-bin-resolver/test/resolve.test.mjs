/**
 * Tests for @reddb-io/internal-bin-resolver.
 *
 * Run: node test/resolve.test.mjs
 */

import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { resolveBin } from '../src/index.js'

let passed = 0
let failed = 0

function test(name, fn) {
  try {
    fn()
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

function withTempPkg(setup) {
  const root = mkdtempSync(join(tmpdir(), 'binres-'))
  try {
    setup(root)
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
    fn()
  } finally {
    for (const k of Object.keys(extra)) {
      if (SAVED_ENV[k] === undefined) delete process.env[k]
      else process.env[k] = SAVED_ENV[k]
    }
  }
}

console.log('@reddb-io/internal-bin-resolver tests')

test('env override is returned verbatim without existence probe', () => {
  withTempPkg((root) => {
    withEnv({ FAKE_BIN: '/nonexistent/path/to/red' }, () => {
      const result = resolveBin({ name: 'red', packageRoot: root, envVar: 'FAKE_BIN' })
      assertEqual(result, '/nonexistent/path/to/red', 'env path returned verbatim')
    })
  })
})

test('env unset falls through to <packageRoot>/bin/<name>', () => {
  withTempPkg((root) => {
    const binDir = join(root, 'bin')
    mkdirSync(binDir, { recursive: true })
    const localPath = join(binDir, 'red')
    writeFileSync(localPath, '')
    withEnv({ FAKE_BIN: undefined }, () => {
      const result = resolveBin({ name: 'red', packageRoot: root, envVar: 'FAKE_BIN' })
      assertEqual(result, localPath, 'local path resolved')
    })
  })
})

test('missing local binary throws actionable error', () => {
  withTempPkg((root) => {
    withEnv({ FAKE_BIN: undefined }, () => {
      assertThrows(
        () => resolveBin({ name: 'red', packageRoot: root, envVar: 'FAKE_BIN' }),
        (err) => {
          const m = err.message
          return (
            m.includes('FAKE_BIN') &&
            m.includes(join(root, 'bin', 'red')) &&
            m.includes('pnpm install')
          )
        },
        'error names env var, expected path, and pnpm install hint',
      )
    })
  })
})

test('empty env value falls through to local', () => {
  withTempPkg((root) => {
    const binDir = join(root, 'bin')
    mkdirSync(binDir, { recursive: true })
    const localPath = join(binDir, 'red')
    writeFileSync(localPath, '')
    withEnv({ FAKE_BIN: '' }, () => {
      const result = resolveBin({ name: 'red', packageRoot: root, envVar: 'FAKE_BIN' })
      assertEqual(result, localPath, 'empty env treated as unset')
    })
  })
})

test('rejects missing required arg', () => {
  assertThrows(
    () => resolveBin({ packageRoot: '/x', envVar: 'X' }),
    (err) => /name/i.test(err.message),
    'missing name throws',
  )
  assertThrows(
    () => resolveBin({ name: 'red', envVar: 'X' }),
    (err) => /packageRoot/i.test(err.message),
    'missing packageRoot throws',
  )
  assertThrows(
    () => resolveBin({ name: 'red', packageRoot: '/x' }),
    (err) => /envVar/i.test(err.message),
    'missing envVar throws',
  )
})

test('PATH is never consulted', () => {
  withTempPkg((root) => {
    // Put a fake "red" on PATH; resolver must still throw because local missing
    // and env unset.
    const pathDir = mkdtempSync(join(tmpdir(), 'binres-path-'))
    try {
      writeFileSync(join(pathDir, 'red'), '')
      withEnv({ FAKE_BIN: undefined, PATH: pathDir }, () => {
        assertThrows(
          () => resolveBin({ name: 'red', packageRoot: root, envVar: 'FAKE_BIN' }),
          (err) => err.message.includes(join(root, 'bin', 'red')),
          'PATH ignored',
        )
      })
    } finally {
      rmSync(pathDir, { recursive: true, force: true })
    }
  })
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed > 0 ? 1 : 0)
