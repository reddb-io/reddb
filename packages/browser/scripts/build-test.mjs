import { execFileSync } from 'node:child_process'
import { readFileSync, mkdirSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const toolchain = readFileSync(resolve(root, 'rust-toolchain.toml'), 'utf8').match(/channel\s*=\s*"([^"]+)"/)[1]
execFileSync('cargo', [`+${toolchain}`, 'build', '--locked', '-p', 'reddb-io-engine', '--example', 'browser_conformance', '--target', 'wasm32-unknown-unknown', '--release'], { cwd: root, stdio: 'inherit' })
const metadata = JSON.parse(execFileSync('cargo', [`+${toolchain}`, 'metadata', '--format-version', '1', '--no-deps'], { cwd: root, encoding: 'utf8' }))
const out = resolve(root, 'packages/browser/test/wasm')
mkdirSync(out, { recursive: true })
execFileSync(process.env.WASM_BINDGEN || 'wasm-bindgen', [
  resolve(metadata.target_directory, 'wasm32-unknown-unknown/release/examples/browser_conformance.wasm'),
  '--target', 'web', '--out-dir', out, '--out-name', 'conformance',
], { cwd: root, stdio: 'inherit' })
execFileSync('cargo', [`+${toolchain}`, 'build', '--locked', '-p', 'reddb-io-wasm', '--example', 'pager_conformance', '--target', 'wasm32-unknown-unknown', '--release'], { cwd: root, stdio: 'inherit' })
execFileSync(process.env.WASM_BINDGEN || 'wasm-bindgen', [
  resolve(metadata.target_directory, 'wasm32-unknown-unknown/release/examples/pager_conformance.wasm'),
  '--target', 'web', '--out-dir', out, '--out-name', 'pager',
], { cwd: root, stdio: 'inherit' })
