#!/usr/bin/env node
'use strict'

/**
 * Syncs the version from root package.json across every publishable
 * artifact in this repo.
 *
 * Runs automatically via the npm "version" lifecycle hook:
 *   pnpm version patch
 *     1. pnpm bumps package.json (e.g. 0.2.4 → 0.2.5)
 *     2. pnpm runs this script
 *     3. We update every manifest + lockfile to match
 *     4. We stage the changed files so pnpm includes them in the
 *        version commit
 *     5. pnpm commits + creates the git tag
 *
 * After this, `git push --follow-tags` triggers the release workflow
 * and the engine + driver versions stay locked in sync.
 *
 * Files updated:
 *   - Cargo.toml                      (engine crate)
 *   - Cargo.lock                      (regenerated)
 *   - drivers/rust/Cargo.toml         (reddb-client)
 *   - drivers/rust/Cargo.lock         (regenerated)
 *   - drivers/js/package.json         (reddb npm)
 *   - drivers/python/Cargo.toml       (reddb-python internal name)
 *   - drivers/python/Cargo.lock       (regenerated)
 *   - drivers/python/pyproject.toml   (reddb PyPI)
 *
 * Source of truth: the root npm manifest's `version` field.
 */

const { execSync } = require('node:child_process')
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

// Regenerate every Cargo.lock so `cargo build --locked` (used by
// CI + crates.io publish) doesn't bail on a version mismatch.
// Best-effort — if cargo is missing or fails for unrelated
// reasons, we still proceed and let the release workflow do
// the authoritative verify.
const lockManifests = [
  path.join(root, 'Cargo.toml'),
  path.join(root, 'drivers', 'rust', 'Cargo.toml'),
  path.join(root, 'drivers', 'python', 'Cargo.toml'),
]

for (const manifest of lockManifests) {
  if (!fs.existsSync(manifest)) continue
  try {
    execSync(`cargo generate-lockfile --manifest-path "${manifest}"`, {
      stdio: 'pipe',
      timeout: 120_000,
    })
  } catch (err) {
    console.warn(`  WARN regenerate-lockfile ${manifest}: ${err.message.split('\n')[0]}`)
  }
}

// Stage every file that the version bump touches so pnpm's
// version commit picks them up in one atomic commit.
const stageList = [
  'package.json',
  'Cargo.toml',
  'Cargo.lock',
  'drivers/rust/Cargo.toml',
  'drivers/rust/Cargo.lock',
  'drivers/js/package.json',
  'drivers/python/Cargo.toml',
  'drivers/python/Cargo.lock',
  'drivers/python/pyproject.toml',
]
  .map((f) => path.join(root, f))
  .filter((f) => fs.existsSync(f))

if (stageList.length > 0) {
  try {
    execSync(`git add ${stageList.map((f) => `"${f}"`).join(' ')}`, {
      cwd: root,
      stdio: 'pipe',
    })
  } catch (err) {
    console.warn(`  WARN git add: ${err.message.split('\n')[0]}`)
  }
}

console.log(`  staged ${stageList.length} files for the version commit`)

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
