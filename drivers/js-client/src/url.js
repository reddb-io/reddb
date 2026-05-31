/**
 * Compatibility shim. The `red://` connection-string parser now lives in
 * the transport-agnostic core (`core/url.js`); this file keeps the
 * historical `./url.js` import path working.
 */

export {
  parseUri,
  parseRedUrl,
  parseLegacyUrl,
  deriveLoginUrl,
} from './core/url.js'
