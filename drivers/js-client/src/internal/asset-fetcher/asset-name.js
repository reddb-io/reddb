/**
 * Resolve the GitHub release asset filename for a given Node
 * `process.platform` / `process.arch` pair and binary base name.
 *
 * Mapping is the existing scheme from `drivers/js/postinstall.js`:
 *   linux  + x64       → <bin>-linux-x86_64
 *   linux  + arm64     → <bin>-linux-aarch64
 *   linux  + arm/armv7l→ <bin>-linux-armv7
 *   darwin + x64       → <bin>-macos-x86_64
 *   darwin + arm64     → <bin>-macos-aarch64
 *   win32  + x64       → <bin>-windows-x86_64.exe
 *
 * Throws `UnsupportedPlatformError` for any other combination.
 */

export class UnsupportedPlatformError extends Error {
  constructor(platform, arch) {
    super(`unsupported platform/arch combination: ${platform}/${arch}`)
    this.name = 'UnsupportedPlatformError'
    this.code = 'UNSUPPORTED_PLATFORM'
    this.platform = platform
    this.arch = arch
  }
}

export function composeAssetName({ platform, arch, binName }) {
  if (typeof binName !== 'string' || binName === '') {
    throw new TypeError('composeAssetName: `binName` must be a non-empty string')
  }
  if (platform === 'linux' && arch === 'x64') return `${binName}-linux-x86_64`
  if (platform === 'linux' && arch === 'arm64') return `${binName}-linux-aarch64`
  if (platform === 'linux' && (arch === 'arm' || arch === 'armv7l')) return `${binName}-linux-armv7`
  if (platform === 'darwin' && arch === 'x64') return `${binName}-macos-x86_64`
  if (platform === 'darwin' && arch === 'arm64') return `${binName}-macos-aarch64`
  if (platform === 'win32' && arch === 'x64') return `${binName}-windows-x86_64.exe`
  throw new UnsupportedPlatformError(platform, arch)
}
