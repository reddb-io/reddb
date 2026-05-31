/**
 * Parity proof (#876): the Node and Web streaming implementations are run
 * through the *same* behavioural suite against the *same* mock transport
 * sessions. Both must exhibit identical frame classification, ordering,
 * cancellation, and terminal-envelope behaviour.
 */

import * as nodeStreaming from '../src/streaming.js'
import * as webStreaming from '../src/streaming-web.js'
import { runStreamingParitySuite } from './streaming-parity-suite.mjs'

runStreamingParitySuite('node', nodeStreaming)
runStreamingParitySuite('web', webStreaming)
