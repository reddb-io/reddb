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
5. back off transport/read failures instead of retrying in a hot loop;
6. optionally renew the active delivery lease while the handler runs;
7. repeat until paused or stopped.

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
logical client dedicated to that worker. Two pods therefore already provide
two-way parallelism. The optional bounded `concurrency` setting creates
additional delivery lanes inside one process; increase it only when the handler
and downstream dependencies can sustain the extra load.

Long handlers can enable lease heartbeat. Each heartbeat runs `QUEUE EXTEND`
with the opaque `delivery_id`; the server derives the new deadline from the
queue's current `LOCK_DEADLINE_MS`. Set the heartbeat comfortably below that
deadline. If renewal fails, the worker signals cooperative cancellation to the
handler and sends neither ACK nor NACK because ownership is no longer certain.

Handler timeout follows the same ownership rule. The timeout signals the
handler's cancellation token/abort signal, then waits for the handler to settle
before NACKing. This means a handler that ignores cancellation can delay
shutdown, but RedDB never deliberately overlaps it with a redelivery.

Graceful stop finishes the active handler and its ACK/NACK before returning.
When stopping from inside the handler, use the driver's non-waiting request
method (`requestStop()` in JavaScript and Dart, `RequestStop()` in .NET,
`Stop()` in Go) and await the worker only after the handler returns; awaiting
graceful completion from the handler would wait on itself.
An idle worker can take up to its configured wait duration to observe the stop
request. Drivers whose transport wires request cancellation can interrupt the
outstanding request; otherwise even `abort` is bounded by the configured wait.
An interrupted in-flight delivery remains subject to normal redelivery rules.

## Rolling deployment order

Deploy the RedDB server before deploying workers built with this SDK release.
The new server understands `QUEUE EXTEND`; older clients continue to work
because their READ/ACK/NACK protocol is unchanged. A worker with heartbeat
enabled against an older server stops on its first renewal attempt and sends
neither ACK nor NACK. A worker without heartbeat can still run against an older
server, provided that server already returns the opaque `delivery_id`.

After every target server is upgraded, roll the application pods normally.
Mixed old/new application pods are safe against the new server. Rolling back
only the application is also safe because old clients ignore the additive
column. Do not roll the server back while new workers are running.

## Language APIs

All official SDKs expose the same lifecycle using the idioms of the language.

| SDK | Scoped queue and worker entry point |
| --- | --- |
| JavaScript / TypeScript / Bun | `db.queue('jobs').consumer(options).start()` |
| Rust | `db.queue_scope("jobs")?.worker(QueueWorkerOptions::new("worker"))?.run(handler).await` |
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

### Runtime capability details

The settlement guarantees are identical, but cancellation mechanics follow
what each runtime can safely provide:

| SDK | Lanes | Lease heartbeat | Handler timeout / cancellation |
| --- | ---: | --- | --- |
| JavaScript / TypeScript / Bun | 1–128 | automatic | `AbortSignal`; waits for settlement |
| Rust | 1–128 | automatic | drops the handler future before NACK |
| Go | 1–128 | automatic | cancels handler `context.Context`; waits |
| Python asyncio | 1–128 | automatic | cancels and joins the asyncio task |
| Python embedded | 1 per worker thread | automatic Rust monitor | waits for the Python callback; Python callbacks are not force-killed |
| .NET | 1–128 | automatic | cancels handler token; waits |
| Java | 1–128 | automatic | interrupts cooperatively; waits |
| Kotlin | 1–128 | automatic | cancels and joins the coroutine |
| Dart | 1–128 | automatic | contextual handler sees `isCancelled`; waits |
| C++ | 1–128 | automatic | contextual handler sees `cancelled()`; waits |
| PHP | 1 per worker process | cooperative `checkpoint()` / `sleep()` | measured after synchronous handler settles |
| Zig | 1 per consumer thread | automatic | optional atomic cancellation flag; joins handler thread |

PHP deliberately uses process-level parallelism: run another worker process
instead of setting in-process concurrency. Its `QueueHandlerContext` offers
`extendLease()`, `checkpoint()`, and a heartbeat-aware `sleep()` for long
synchronous handlers. Zig uses the same explicit model at thread level. These
limits are surfaced by validation instead of silently pretending the runtime
can preempt arbitrary user code.

Across every SDK, an ambiguous ACK error is reported but is never followed by
a NACK. A failed lease renewal also produces no settlement command. These two
rules prevent the client from acting on a delivery it may no longer own.

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
  concurrency: 1,
  leaseHeartbeatMs: 10_000,
  handlerTimeoutMs: 120_000,
  receiveBackoff: { initialMs: 250, maxMs: 30_000, jitter: true },
  async handleMessage(message, { signal }) {
    await sendEmail(message.payload, { signal })
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
