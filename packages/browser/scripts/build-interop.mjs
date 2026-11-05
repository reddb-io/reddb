import { execFileSync } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const output = resolve(process.env.REDDB_BROWSER_INTEROP_DIR || resolve(root, 'packages/browser/test/interop-build'))
mkdirSync(resolve(output, 'dart'), { recursive: true })
execFileSync(process.env.DART || 'dart', ['pub', 'get'], { cwd: resolve(root, 'drivers/dart'), stdio: 'inherit' })
execFileSync(process.env.DART || 'dart', [
  'compile', 'js', 'drivers/dart/test/browser_runtime_probe.dart', '-o', resolve(output, 'dart/main.js'),
], { cwd: root, stdio: 'inherit' })
execFileSync(process.env.DOTNET || 'dotnet', [
  'build', 'drivers/dotnet/tests/BrowserHarness/BrowserHarness.csproj',
  '--artifacts-path', resolve(output, 'dotnet'), '-p:UseSharedCompilation=false',
], { cwd: root, stdio: 'inherit' })
