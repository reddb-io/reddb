import { execFileSync } from 'node:child_process'
import { readFileSync, mkdirSync, copyFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const toolchain = readFileSync(resolve(root, 'rust-toolchain.toml'), 'utf8').match(/channel\s*=\s*"([^"]+)"/)[1]
const command = ['--locked', '-p', 'reddb-io-wasm', '--target', 'wasm32-unknown-unknown', '--release']
execFileSync('cargo', [`+${toolchain}`, 'build', ...command], { cwd: root, stdio: 'inherit' })
const metadata = JSON.parse(execFileSync('cargo', [`+${toolchain}`, 'metadata', '--format-version', '1', '--no-deps'], { cwd: root, encoding: 'utf8' }))
const out = resolve(root, 'packages/browser/src/wasm')
mkdirSync(out, { recursive: true })
execFileSync(process.env.WASM_BINDGEN || 'wasm-bindgen', [
  resolve(metadata.target_directory, 'wasm32-unknown-unknown/release/reddb_wasm.wasm'),
  '--target', 'web', '--out-dir', out, '--out-name', 'reddb_wasm',
], { cwd: root, stdio: 'inherit' })

copyFileSync(resolve(root, 'LICENSE'), resolve(root, 'packages/browser/LICENSE'))
