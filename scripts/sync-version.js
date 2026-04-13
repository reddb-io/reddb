#!/usr/bin/env node
'use strict'

/**
 * Syncs the version from root package.json across every publishable
 * artifact in this repo.
 *
 * Runs automatically via the npm "version" lifecycle hook:
 *   pnpm version patch
 *     → package.json bumped
 *     → this script
 *     → every manifest updated
 *     → git add
 *
 * Files updated:
 *   - Cargo.toml                      (engine crate)
 *   - drivers/rust/Cargo.toml         (reddb-client)
 *   - drivers/js/package.json         (reddb npm)
 *   - drivers/python/Cargo.toml       (reddb-python internal name)
 *   - drivers/python/pyproject.toml   (reddb PyPI)
 *
 * Reads version from package.json. Source of truth: the root npm manifest.
 */

const fs = require('node:fs')
const path = require('node:path')

const root = path.resolve(__dirname, '..')

const pkgPath = path.join(root, 'package.json')
const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'))
const version = pkg.version

if (!version || !/^\d+\.\d+\.\d+/.test(version)) {
  console.error(`Invalid version in package.json: ${version}`)
  process.exit(1)
}

const targets = [
  {
    label: 'Cargo.toml',
    file: path.join(root, 'Cargo.toml'),
    type: 'cargo-toml',
  },
  {
    label: 'drivers/rust/Cargo.toml',
    file: path.join(root, 'drivers', 'rust', 'Cargo.toml'),
    type: 'cargo-toml',
  },
  {
    label: 'drivers/js/package.json',
    file: path.join(root, 'drivers', 'js', 'package.json'),
    type: 'package-json',
  },
  {
    label: 'drivers/python/Cargo.toml',
    file: path.join(root, 'drivers', 'python', 'Cargo.toml'),
    type: 'cargo-toml',
  },
  {
    label: 'drivers/python/pyproject.toml',
    file: path.join(root, 'drivers', 'python', 'pyproject.toml'),
    type: 'pyproject-toml',
  },
]

const changes = []
let failed = 0

for (const t of targets) {
  try {
    const before = apply(t)
    changes.push({ label: t.label, before, after: version })
  } catch (err) {
    failed++
    console.error(`  FAIL ${t.label}: ${err.message}`)
  }
}

console.log(`synced version: → ${version}`)
for (const c of changes) {
  const arrow = c.before === version ? '=' : '→'
  console.log(`  ${c.label.padEnd(32)} ${c.before} ${arrow} ${c.after}`)
}

if (failed > 0) {
  process.exit(1)
}

function apply(target) {
  if (!fs.existsSync(target.file)) {
    throw new Error(`missing file`)
  }
  const original = fs.readFileSync(target.file, 'utf8')
  let before = null
  let updated = null

  if (target.type === 'cargo-toml') {
    const match = original.match(/^version = "(.+?)"/m)
    if (!match) throw new Error(`no top-level version line`)
    before = match[1]
    updated = original.replace(/^version = ".*"/m, `version = "${version}"`)
  } else if (target.type === 'package-json') {
    const json = JSON.parse(original)
    before = json.version ?? null
    json.version = version
    updated = JSON.stringify(json, null, 2) + '\n'
  } else if (target.type === 'pyproject-toml') {
    const match = original.match(/^version = "(.+?)"/m)
    if (!match) throw new Error(`no top-level version line in [project]`)
    before = match[1]
    updated = original.replace(/^version = ".*"/m, `version = "${version}"`)
  } else {
    throw new Error(`unknown target type: ${target.type}`)
  }

  if (updated !== original) {
    fs.writeFileSync(target.file, updated)
  }
  return before
}
