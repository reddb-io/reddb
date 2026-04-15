#!/usr/bin/env node

import { spawn } from 'node:child_process'
import { resolveBinaryPath } from './binary.js'

const binary = resolveBinaryPath()
const args = process.argv.slice(2)

const child = spawn(binary, args, {
  cwd: process.cwd(),
  env: process.env,
  stdio: 'inherit',
})

child.on('error', (err) => {
  process.stderr.write(`reddb-cli: ${err.message}\n`)
  process.exit(1)
})

child.on('exit', (code, signal) => {
  if (signal) {
    process.exit(1)
  }
  process.exit(code ?? 0)
})
