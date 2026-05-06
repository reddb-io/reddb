/**
 * Tests for @reddb-io/internal-version-compare.
 *
 * Run: node test/compare.test.mjs
 */

import { compareInstalled, parseVersion, compareSemver } from '../src/index.js'

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

function assertEqual(actual, expected, msg) {
  if (actual !== expected) {
    throw new Error(`${msg}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`)
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(`assertion failed: ${msg}`)
}

const ok = (out) => () => out
const fail = (msg) => () => {
  throw new Error(msg)
}

console.log('@reddb-io/internal-version-compare tests')

// ---- parseVersion ----

test('parseVersion strips `reddb` prefix', () => {
  assertEqual(parseVersion('reddb 0.2.9'), '0.2.9', 'reddb prefix stripped')
})

test('parseVersion accepts bare semver', () => {
  assertEqual(parseVersion('0.2.9'), '0.2.9', 'bare semver returned')
})

test('parseVersion accepts prerelease', () => {
  assertEqual(parseVersion('reddb 1.0.0-rc.1'), '1.0.0-rc.1', 'prerelease preserved')
})

test('parseVersion returns null on garbage', () => {
  assertEqual(parseVersion('not-a-version'), null, 'garbage rejected')
  assertEqual(parseVersion(''), null, 'empty rejected')
})

// ---- compareSemver ----

test('compareSemver: equal versions', () => {
  assertEqual(compareSemver('1.2.3', '1.2.3'), 0, 'equal')
})

test('compareSemver: numeric ordering', () => {
  assert(compareSemver('1.2.3', '1.2.4') < 0, '1.2.3 < 1.2.4')
  assert(compareSemver('1.3.0', '1.2.9') > 0, '1.3.0 > 1.2.9')
  assert(compareSemver('2.0.0', '1.99.99') > 0, '2.0.0 > 1.99.99')
})

test('compareSemver: prerelease < release', () => {
  assert(compareSemver('1.0.0-rc.1', '1.0.0') < 0, 'rc < release')
  assert(compareSemver('1.0.0', '1.0.0-rc.1') > 0, 'release > rc')
})

test('compareSemver: prerelease ordering', () => {
  assert(compareSemver('1.0.0-alpha', '1.0.0-beta') < 0, 'alpha < beta')
  assert(compareSemver('1.0.0-rc.1', '1.0.0-rc.2') < 0, 'rc.1 < rc.2')
  assert(compareSemver('1.0.0-rc.2', '1.0.0-rc.10') < 0, 'numeric prerelease compares numerically')
})

// ---- compareInstalled ----

test('equal versions → skip', () => {
  const v = compareInstalled({ packageVersion: '0.2.9', exec: ok('reddb 0.2.9') })
  assertEqual(v.action, 'skip', 'action')
  assert(v.reason.includes('0.2.9'), 'reason mentions version')
})

test('package newer than PATH → upgrade', () => {
  const v = compareInstalled({ packageVersion: '0.3.0', exec: ok('reddb 0.2.9') })
  assertEqual(v.action, 'upgrade', 'action')
  assert(v.reason.includes('0.2.9'), 'reason mentions installed')
  assert(v.reason.includes('0.3.0'), 'reason mentions package')
})

test('package older than PATH → skip', () => {
  const v = compareInstalled({ packageVersion: '0.2.9', exec: ok('reddb 0.3.0') })
  assertEqual(v.action, 'skip', 'action')
  assert(/newer/i.test(v.reason), 'reason explains PATH newer')
})

test('exec failure (no PATH binary) → install', () => {
  const v = compareInstalled({ packageVersion: '0.2.9', exec: fail('command not found') })
  assertEqual(v.action, 'install', 'action')
  assert(/not.*detected|no.*binary|command not found/i.test(v.reason), 'reason explains missing')
})

test('malformed PATH version → install (warn)', () => {
  const v = compareInstalled({ packageVersion: '0.2.9', exec: ok('garbage output') })
  assertEqual(v.action, 'install', 'action')
  assert(/unparseable|malformed/i.test(v.reason), 'reason explains malformed')
})

test('prerelease ordering — release package vs PATH prerelease → upgrade', () => {
  const v = compareInstalled({ packageVersion: '1.0.0', exec: ok('reddb 1.0.0-rc.1') })
  assertEqual(v.action, 'upgrade', 'release > rc.1')
})

test('prerelease ordering — rc package vs PATH release → skip', () => {
  const v = compareInstalled({ packageVersion: '1.0.0-rc.1', exec: ok('reddb 1.0.0') })
  assertEqual(v.action, 'skip', 'release on PATH wins')
})

test('rejects missing required arg', () => {
  let threw = false
  try {
    compareInstalled({ exec: ok('reddb 0.2.9') })
  } catch (err) {
    threw = /packageVersion/.test(err.message)
  }
  assert(threw, 'missing packageVersion throws')

  threw = false
  try {
    compareInstalled({ packageVersion: '0.2.9' })
  } catch (err) {
    threw = /exec/.test(err.message)
  }
  assert(threw, 'missing exec throws')
})

test('invalid packageVersion throws', () => {
  let threw = false
  try {
    compareInstalled({ packageVersion: 'not-a-version', exec: ok('reddb 0.2.9') })
  } catch (err) {
    threw = /packageVersion/.test(err.message)
  }
  assert(threw, 'unparseable packageVersion throws (programmer error)')
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed > 0 ? 1 : 0)
