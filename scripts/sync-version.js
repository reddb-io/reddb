#!/usr/bin/env node
'use strict';

/**
 * Syncs the version from package.json → Cargo.toml
 *
 * Runs automatically via the npm "version" lifecycle hook:
 *   pnpm version patch  →  package.json bumped  →  this script  →  Cargo.toml updated  →  git add
 *
 * This ensures: package.json = Cargo.toml = npm = crates.io = GitHub release = binary
 */

const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const pkgPath = path.join(root, 'package.json');
const cargoPath = path.join(root, 'Cargo.toml');

const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
const version = pkg.version;

if (!version || !/^\d+\.\d+\.\d+/.test(version)) {
  console.error(`Invalid version in package.json: ${version}`);
  process.exit(1);
}

// Update Cargo.toml
let cargo = fs.readFileSync(cargoPath, 'utf8');
const before = cargo.match(/^version = "(.+?)"/m)?.[1];
cargo = cargo.replace(/^version = ".*"/m, `version = "${version}"`);
fs.writeFileSync(cargoPath, cargo);

console.log(`synced version: ${before} → ${version}`);
console.log(`  package.json: ${version}`);
console.log(`  Cargo.toml:   ${version}`);
