import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../../../', import.meta.url))
const toolchain = readFileSync(resolve(root, 'rust-toolchain.toml'), 'utf8').match(/channel\s*=\s*"([^"]+)"/)[1]
execFileSync('cargo', [`+${toolchain}`, 'build', '--locked', '-p', 'reddb-io-wire', '--example', 'client_codec', '--target', 'wasm32-unknown-unknown', '--release'], { cwd: root, stdio: 'inherit' })
const metadata = JSON.parse(execFileSync('cargo', [`+${toolchain}`, 'metadata', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8' }))
const out = resolve(root, 'drivers/js-client/bench/wasm')
mkdirSync(out, { recursive: true })
execFileSync(process.env.WASM_BINDGEN || 'wasm-bindgen', [resolve(metadata.target_directory, 'wasm32-unknown-unknown/release/examples/client_codec.wasm'), '--target', 'web', '--out-dir', out, '--out-name', 'client_codec'], { cwd: root, stdio: 'inherit' })
