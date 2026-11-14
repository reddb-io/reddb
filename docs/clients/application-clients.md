# Application clients and queue workers

RedDB SDKs expose application-scoped clients for state that belongs to a
named collection or queue. Bind the scope once, then pass that handle through
the application:

```javascript
const jobs = db.queue('jobs')
const config = db.kv('runtime_config')
const secrets = db.secrets('payments')
const cache = db.cache('sessions')
```

The explicit name is part of the client identity. It prevents a handler from
reading from one queue and acknowledging another, and prevents a component
from silently falling back to a global KV, Vault, or cache namespace. Legacy
unscoped helpers remain available where a driver already exposed them, but new
application code should use an explicit scope.

## Queue worker contract

A queue worker is a long-running, asynchronous processing loop comparable to
a Kafka consumer, RabbitMQ consumer, or `sqs-consumer`:

1. issue one server-side `QUEUE READ ... WAIT` for one delivery;
2. invoke the application handler;
3. `ACK` the opaque `delivery_id` only after the handler succeeds;
4. `NACK` the delivery when the handler fails, optionally with a retry delay;
5. repeat until stopped.

This is not interval polling. `WAIT` parks the request in RedDB until a
message is available or the bounded wait expires. The JavaScript remote
client uses the native RedWire queue-wait stream when the negotiated transport
supports it. The other drivers issue the equivalent SQL wait request; the
waiting still happens in the server.

Delivery is **at least once**. A process can fail after its side effect and
before `ACK`, so handlers must be idempotent. The server-issued `delivery_id`
is opaque and must be returned unchanged. Do not acknowledge a message when it
is received and do not derive an ACK handle from the payload or message ID.

Workers process one delivery at a time by default. Run one worker per pod on a
connection dedicated to that worker. Two pods therefore provide two-way
parallelism without concurrent handlers inside either process. Add more worker
instances only when the handler and its downstream dependencies can sustain
the extra concurrency.

Graceful stop finishes the active handler and its ACK/NACK before returning.
An idle worker can take up to its configured wait duration to observe the stop
request. Drivers whose transport wires request cancellation can interrupt the
outstanding request; otherwise even `abort` is bounded by the configured wait.
An interrupted in-flight delivery remains subject to normal redelivery rules.

## Rolling deployment order

Deploy the RedDB server before deploying workers built with this SDK release.
The server adds `delivery_id` to the existing `QUEUE READ` result and native
RedWire queue-wait event; this is an additive column, so older clients continue
to work. A new worker connected to an older server deliberately stops with an
`INVALID_RESPONSE`-style error before invoking the handler because it cannot
safely ACK without the opaque handle.

After every target server is upgraded, roll the application pods normally.
Mixed old/new application pods are safe against the new server. Rolling back
only the application is also safe because old clients ignore the additive
column. Do not roll the server back while new workers are running.

## Language APIs

All official SDKs expose the same lifecycle using the idioms of the language.

| SDK | Scoped queue and worker entry point |
| --- | --- |
| JavaScript / TypeScript / Bun | `db.queue('jobs').consumer(options).start()` |
| Rust | `db.queue_scope("jobs")?.consume(options, stopping, handler).await` |
| Go | `helpers.QueueScope("jobs")`, then `Consumer(options).Start(ctx)` |
| Python asyncio | `db.queue("jobs").consumer(...).start()` |
| Python embedded | `db.queue("jobs").consumer(...)`; run with `asyncio.to_thread(worker.run, handler)` or a dedicated thread |
| .NET | `helpers.Queue("jobs").Consumer(options).StartAsync()` |
| Java | `helpers.queue("jobs").consumer(options).start()` |
| Kotlin | `helpers.queue("jobs").consumer(options).start(scope)` |
| PHP | `$helpers->queue('jobs')->consumer($handler, $options)->run()` in a dedicated worker process |
| Dart | `helpers.queueNamed('jobs').consumer(options).start()` |
| C++ | `QueueConsumer worker(helpers.queue("jobs"), options); worker.start();` |
| Zig | call `helpers.queueNamed("jobs").consume(options)` in a spawned thread |

The worker options consistently identify the consumer, optional consumer
group, bounded wait, retry delay, handler, and optional error callback. Exact
type names and casing follow each language's conventions.

## KV, secrets, and cache scopes

KV and secrets use the same bind-once pattern:

| SDK family | KV scope | Secret scope |
| --- | --- | --- |
| JavaScript / TypeScript / Bun | `db.kv('runtime')` | `db.secrets('payments')` |
| Rust | `db.kv_collection("runtime")` | `db.secrets("payments")` |
| Go | `helpers.KVScope("runtime")` | `helpers.Secrets("payments")` |
| Python asyncio | `db.kv("runtime")` | `db.secrets("payments")` |
| Python embedded | KV operations take the collection explicitly | `db.secrets("payments")` |
| .NET | `helpers.Kv("runtime")` | `helpers.Secrets("payments")` |
| Java / Kotlin | `helpers.kv("runtime")` | `helpers.secrets("payments")` |
| PHP | `$helpers->kv('runtime')` | `$helpers->secrets('payments')` |
| Dart | `helpers.kvNamed('runtime')` | `helpers.secrets('payments')` |
| C++ | `helpers.kv("runtime")` | `helpers.secrets("payments")` |
| Zig | `helpers.kv("runtime")` | `helpers.secrets("payments")` |

Cache is not a SQL data model, so it cannot be truthfully implemented by the
generic query-only helper used by every driver. Scoped cache handles are
currently available on the JavaScript remote and embedded SDK surfaces;
namespace-aware cache operations are also available in Python embedded.
Other SDKs do not pretend cache parity until their transport has a real cache
operation.

## JavaScript example

```javascript
const worker = db.queue('emails').consumer({
  consumer: `mailer-${process.env.HOSTNAME}`,
  group: 'mailers',
  waitMs: 30_000,
  retryDelay: '5s',
  async handleMessage(message) {
    await sendEmail(message.payload)
  },
  async onError(error, message) {
    console.error('queue handler failed', { error, deliveryId: message?.delivery_id })
  },
})

worker.start()

process.once('SIGTERM', async () => {
  await worker.stop()
  await db.close()
})
```

Create the queue and consumer group before starting application workers. See
the [queue quickstart](../getting-started/quickstart-queue.md) for the server
setup and [Queues & Deques](../data-models/queues.md) for retry, DLQ, and
delivery semantics.
