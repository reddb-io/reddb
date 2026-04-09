#!/usr/bin/env node
'use strict';

const assert = require('node:assert');
const path = require('node:path');
const sdk = require('../red-sdk');

const { version, resolveAssetName, createClient } = sdk;

// Test 1: SDK version matches package.json
const pkg = require(path.join(__dirname, '..', '..', 'package.json'));
assert.strictEqual(version, pkg.version, 'SDK version should match package.json');
console.log('PASS: SDK version matches package.json (%s)', version);

// Test 2: Asset name resolution - Linux x64
const linuxX64 = resolveAssetName({ platform: 'linux', arch: 'x64' });
assert.strictEqual(linuxX64, 'red-linux-x86_64');
console.log('PASS: Linux x64 asset = %s', linuxX64);

// Test 3: Asset name resolution - macOS arm64
const macArm64 = resolveAssetName({ platform: 'darwin', arch: 'arm64' });
assert.strictEqual(macArm64, 'red-macos-aarch64');
console.log('PASS: macOS arm64 asset = %s', macArm64);

// Test 4: Asset name resolution - macOS x64
const macX64 = resolveAssetName({ platform: 'darwin', arch: 'x64' });
assert.strictEqual(macX64, 'red-macos-x86_64');
console.log('PASS: macOS x64 asset = %s', macX64);

// Test 5: Asset name resolution - Windows x64
const winX64 = resolveAssetName({ platform: 'win32', arch: 'x64' });
assert.strictEqual(winX64, 'red-windows-x86_64.exe');
console.log('PASS: Windows x64 asset = %s', winX64);

// Test 6: Asset name resolution - Linux arm64
const linuxArm64 = resolveAssetName({ platform: 'linux', arch: 'arm64' });
assert.strictEqual(linuxArm64, 'red-linux-aarch64');
console.log('PASS: Linux arm64 asset = %s', linuxArm64);

// Test 7: Asset name resolution - Linux arm64 static
const linuxArm64Static = resolveAssetName({ platform: 'linux', arch: 'arm64', staticBuild: true });
assert.strictEqual(linuxArm64Static, 'red-linux-aarch64-static');
console.log('PASS: Linux arm64 static asset = %s', linuxArm64Static);

// Test 8: Asset name resolution - Linux armv7
const linuxArmv7 = resolveAssetName({ platform: 'linux', arch: 'arm' });
assert.strictEqual(linuxArmv7, 'red-linux-armv7');
console.log('PASS: Linux armv7 asset = %s', linuxArmv7);

// Test 9: Unsupported platform throws
assert.throws(
  () => resolveAssetName({ platform: 'freebsd', arch: 'x64' }),
  /Unsupported reddb platform combination/
);
console.log('PASS: Unsupported platform throws error');

// Test 10: Public exports exist
assert.strictEqual(typeof sdk.createClient, 'function');
assert.strictEqual(typeof sdk.resolveBinary, 'function');
assert.strictEqual(typeof sdk.downloadBinary, 'function');
assert.strictEqual(typeof sdk.ensureInstalled, 'function');
assert.strictEqual(typeof sdk.getBinaryInfo, 'function');
assert.strictEqual(typeof sdk.getInstalledVersion, 'function');
assert.strictEqual(typeof sdk.execFile, 'function');
assert.strictEqual(typeof sdk.runJson, 'function');
assert.strictEqual(typeof sdk.spawnChild, 'function');
assert.strictEqual(typeof sdk.runCli, 'function');
assert.strictEqual(typeof sdk.checkForUpdates, 'function');
assert.strictEqual(typeof sdk.upgradeBinary, 'function');
assert.strictEqual(typeof sdk.resolveBinaryWithInfo, 'function');
console.log('PASS: All public exports are functions');

// Test 11: Internal exports exist
assert.ok(sdk._internal, '_internal namespace should exist');
assert.strictEqual(typeof sdk._internal.buildFlags, 'function');
assert.strictEqual(typeof sdk._internal.exists, 'function');
assert.strictEqual(typeof sdk._internal.parseInstalledVersion, 'function');
assert.strictEqual(typeof sdk._internal.splitWrapperArgs, 'function');
console.log('PASS: Internal exports accessible');

// Test 12: buildFlags helper
const flags = sdk._internal.buildFlags({ json: true, port: 8080, verbose: false, tags: ['a', 'b'] });
assert.deepStrictEqual(flags, ['--json', '--port', '8080', '--tags', 'a', '--tags', 'b']);
console.log('PASS: buildFlags produces correct args');

// Test 13: parseInstalledVersion
assert.strictEqual(sdk._internal.parseInstalledVersion('red v0.1.0'), 'v0.1.0');
assert.strictEqual(sdk._internal.parseInstalledVersion('0.2.3'), 'v0.2.3');
assert.strictEqual(sdk._internal.parseInstalledVersion(''), null);
assert.strictEqual(sdk._internal.parseInstalledVersion(null), null);
console.log('PASS: parseInstalledVersion parses correctly');

// Test 14: splitWrapperArgs
const split = sdk._internal.splitWrapperArgs(['--install', '--', 'query', 'SELECT 1']);
assert.deepStrictEqual(split.wrapperArgs, ['--install']);
assert.deepStrictEqual(split.passthroughArgs, ['query', 'SELECT 1']);
assert.strictEqual(split.usedDoubleDash, true);
console.log('PASS: splitWrapperArgs splits correctly');

// Test 15: default export
assert.strictEqual(sdk.default, sdk);
console.log('PASS: default export is self-reference');

console.log('\nAll %d tests passed!', 15);
