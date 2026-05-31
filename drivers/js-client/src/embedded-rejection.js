/**
 * Compatibility shim. The embedded-URI rejection logic now lives in the
 * transport-agnostic core (`core/embedded-rejection.js`); this file keeps
 * the historical `./embedded-rejection.js` import path working.
 */

export {
  EMBEDDED_REJECTION_MESSAGE,
  EmbeddedNotSupported,
  isEmbeddedUri,
  rejectEmbeddedUri,
} from './core/embedded-rejection.js'
