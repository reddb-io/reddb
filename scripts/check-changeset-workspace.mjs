#!/usr/bin/env node

import {readdirSync, readFileSync, statSync} from 'node:fs'
import {join, resolve} from 'node:path'

const root = resolve(import.meta.dirname, '..')
const workspace = readFileSync(join(root, 'pnpm-workspace.yaml'), 'utf8')
const patterns = [...workspace.matchAll(/^\s+-\s+["']([^"']+)["']\s*$/gm)].map(match => match[1])

function workspaceDirectories(pattern) {
  if (pattern === '.') return [root]
  if (!pattern.endsWith('/*')) return [join(root, pattern)]
  const parent = join(root, pattern.slice(0, -2))
  return readdirSync(parent)
    .map(name => join(parent, name))
    .filter(path => statSync(path).isDirectory())
}

const packages = new Map()
for (const directory of patterns.flatMap(workspaceDirectories)) {
  let manifest
  try {
    manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'))
  } catch (error) {
    if (error?.code === 'ENOENT') continue
    throw error
  }
  if (typeof manifest.name !== 'string' || typeof manifest.version !== 'string') {
    throw new Error(`${directory}/package.json must declare name and version`)
  }
  packages.set(manifest.name, {directory, manifest})
}

const errors = []
for (const filename of readdirSync(join(root, '.changeset')).filter(name => name.endsWith('.md') && name !== 'README.md')) {
  const source = readFileSync(join(root, '.changeset', filename), 'utf8')
  const frontmatter = source.match(/^---\s*\n([\s\S]*?)\n---(?:\s*\n|$)/)?.[1]
  if (!frontmatter) {
    errors.push(`.changeset/${filename}: missing frontmatter`)
    continue
  }
  for (const line of frontmatter.split('\n').map(value => value.trim()).filter(Boolean)) {
    const entry = line.match(/^["']?([^"':]+)["']?\s*:\s*(patch|minor|major)\s*$/)
    if (!entry) {
      errors.push(`.changeset/${filename}: invalid release entry ${JSON.stringify(line)}`)
    } else if (!packages.has(entry[1])) {
      errors.push(`.changeset/${filename}: ${entry[1]} is not a pnpm workspace package`)
    }
  }
}

const dependencySections = ['dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies']
for (const [consumer, {manifest}] of packages) {
  for (const section of dependencySections) {
    for (const [dependency, specifier] of Object.entries(manifest[section] ?? {})) {
      const target = packages.get(dependency)
      if (!target || typeof specifier !== 'string') continue
      if (specifier.startsWith('workspace:') || specifier === target.manifest.version) continue
      errors.push(`${consumer} ${section}.${dependency} is ${specifier}; use workspace:* or ${target.manifest.version}`)
    }
  }
}

if (errors.length) {
  console.error('changeset workspace contract failed:')
  for (const error of errors) console.error(`  - ${error}`)
  process.exit(1)
}

console.log(`changeset workspace contract passed (${packages.size} packages)`)
