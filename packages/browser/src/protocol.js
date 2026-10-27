export const RUNTIME_ABI_VERSION = 1
export const VALUE_ENCODING = 'reddb-json-v1'
export const MAX_QUERY_PARAMETERS = 65_536

export class BrowserError extends Error {
  constructor(code, message) {
    super(message)
    this.name = 'BrowserError'
    this.code = code
  }
}
