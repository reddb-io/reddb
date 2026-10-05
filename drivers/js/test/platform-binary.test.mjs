import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'

import {
  detectLibc,
  platformPackageNames,
  resolvePlatformBinary,
} from '../src/platform-binary.js'

test('platformPackageNames maps every supported host to its package', () => {
  assert.deepEqual(platformPackageNames({ platform: 'darwin', arch: 'arm64' }), ['@reddb-io/red-darwin-arm64'])
  assert.deepEqual(platformPackageNames({ platform: 'darwin', arch: 'x64' }), ['@reddb-io/red-darwin-x64'])
  assert.deepEqual(platformPackageNames({ platform: 'win32', arch: 'x64' }), ['@reddb-io/red-win32-x64'])
  assert.deepEqual(platformPackageNames({ platform: 'linux', arch: 'arm', libc: 'glibc' }), [
    '@reddb-io/red-linux-arm',
    '@reddb-io/red-linux-arm-musl',
  ])
})

test('linux prefers the matching libc and falls back to the other variant', () => {
  assert.deepEqual(platformPackageNames({ platform: 'linux', arch: 'x64', libc: 'glibc' }), [
    '@reddb-io/red-linux-x64',
    '@reddb-io/red-linux-x64-musl',
  ])
  assert.deepEqual(platformPackageNames({ platform: 'linux', arch: 'arm64', libc: 'musl' }), [
    '@reddb-io/red-linux-arm64-musl',
    '@reddb-io/red-linux-arm64',
  ])
})

test('detectLibc is null off Linux', () => {
  assert.equal(detectLibc('darwin'), null)
  assert.equal(detectLibc('win32'), null)
})

async function fakePackage(root, name, binName) {
  const dir = path.join(root, name)
  await mkdir(path.join(dir, 'bin'), { recursive: true })
  await writeFile(path.join(dir, 'package.json'), '{}')
  await writeFile(path.join(dir, 'bin', binName), '#!/bin/sh\n')
  return path.join(dir, 'bin', binName)
}

test('resolvePlatformBinary returns the installed package binary', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'reddb-platform-'))
  const bin = await fakePackage(root, 'red-linux-x64', 'red')
  const resolve = (id) => {
    if (id === '@reddb-io/red-linux-x64/package.json') return path.join(root, 'red-linux-x64', 'package.json')
    throw new Error('MODULE_NOT_FOUND')
  }
  assert.equal(resolvePlatformBinary({ platform: 'linux', arch: 'x64', libc: 'glibc', resolve }), bin)
})

test('resolvePlatformBinary uses the other libc variant when only it is installed', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'reddb-platform-'))
  const bin = await fakePackage(root, 'red-linux-x64-musl', 'red')
  const resolve = (id) => {
    if (id === '@reddb-io/red-linux-x64-musl/package.json') return path.join(root, 'red-linux-x64-musl', 'package.json')
    throw new Error('MODULE_NOT_FOUND')
  }
  assert.equal(resolvePlatformBinary({ platform: 'linux', arch: 'x64', libc: 'glibc', resolve }), bin)
})

test('resolvePlatformBinary uses red.exe on Windows', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'reddb-platform-'))
  const bin = await fakePackage(root, 'red-win32-x64', 'red.exe')
  const resolve = () => path.join(root, 'red-win32-x64', 'package.json')
  assert.equal(resolvePlatformBinary({ platform: 'win32', arch: 'x64', resolve }), bin)
})

test('resolvePlatformBinary returns null when no platform package is installed', () => {
  const resolve = () => {
    throw new Error('MODULE_NOT_FOUND')
  }
  assert.equal(resolvePlatformBinary({ platform: 'linux', arch: 'x64', libc: 'glibc', resolve }), null)
})
