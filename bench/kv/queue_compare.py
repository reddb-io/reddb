#!/usr/bin/env python3
"""Network queue diagnostics. Every produced payload is checked at consumption.

Batch means outstanding operations, not an atomic transaction. Rabbit consumer
ACK is one-way; Kafka commits offsets and retains records. Do not equate these
completion contracts with RedDB's durable ACK response.
"""
import argparse
import asyncio
import hashlib
import json
import math
import time


def payload(index, size):
    prefix = f"{index:012d}:"
    if size < len(prefix):
        raise ValueError("payload must hold the unique sequence prefix")
    digest = hashlib.sha256(prefix.encode()).hexdigest()
    return (prefix + digest * math.ceil(size / len(digest)))[:size]


def validate(values, indices, size):
    expected = sorted(payload(index, size) for index in indices)
    if sorted(values) != expected:
        raise ValueError("missing, duplicate, or incorrect queue payload")


class RedQueue:
    async def open(self, args, create):
        import aiohttp
        self.args = args
        self.session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=60),
            connector=aiohttp.TCPConnector(limit=args.batch))
        if create:
            await self.query(f"CREATE QUEUE {args.name}")
            await self.query(f"QUEUE GROUP CREATE {args.name} workers")

    async def query(self, sql):
        async with self.session.post(f"http://{self.args.host}:5000/query", json={"query": sql}) as response:
            result = await response.json()
            if response.status != 200 or result.get("ok") is False or "error" in result:
                raise RuntimeError(f"query failed: {response.status}: {result}")
            return [row["values"] for row in result["result"]["records"]]

    async def produce(self, values):
        await asyncio.gather(*(self.query(f"QUEUE PUSH {self.args.name} '{value}'") for value in values))

    async def consume(self, count, empty=False):
        rows = await self.query(f"QUEUE READ {self.args.name} GROUP workers CONSUMER bench COUNT {count}")
        values = [row["payload"] for row in rows]
        if self.args.red_ack_batch and rows:
            ids = ",".join(f"'{row['message_id']}'" for row in rows)
            await self.query(f"QUEUE ACK {self.args.name} BATCH GROUP workers ({ids})")
        else:
            await asyncio.gather(*(self.query(f"QUEUE ACK {self.args.name} GROUP workers '{row['message_id']}'") for row in rows))
        return values

    async def close(self):
        await self.session.close()


class RabbitQueue:
    async def open(self, args, create):
        import aio_pika
        self.connection = await aio_pika.connect(f"amqp://bench:bench@{args.host}/")
        self.channel = await self.connection.channel(publisher_confirms=True, on_return_raises=True)
        self.queue = await self.channel.declare_queue(args.name, durable=True, passive=not create,
                                                    arguments={"x-queue-type": "quorum"})
        await self.channel.set_qos(prefetch_count=args.batch)
        self.messages = asyncio.Queue()
        if args.mode != "seed":
            await self.queue.consume(self.messages.put, no_ack=False)

    async def produce(self, values):
        import aio_pika
        confirms = await asyncio.gather(*(self.channel.default_exchange.publish(
            aio_pika.Message(value.encode(), delivery_mode=aio_pika.DeliveryMode.PERSISTENT),
            routing_key=self.queue.name, mandatory=True) for value in values))
        if any(type(confirm).__name__ != "Ack" for confirm in confirms):
            raise RuntimeError("missing publisher confirmation")

    async def consume(self, count, empty=False):
        values = []
        for _ in range(count):
            try:
                message = await asyncio.wait_for(self.messages.get(), 5 if empty else 60)
            except TimeoutError:
                break
            values.append(message.body.decode())
            await message.ack()
        # An RPC after ACK establishes protocol ordering, not an fsync ACK.
        await self.channel.declare_queue(self.queue.name, passive=True)
        return values

    async def close(self):
        await self.connection.close()


class KafkaQueue:
    async def open(self, args, create):
        from confluent_kafka import Consumer, Producer
        from confluent_kafka.admin import AdminClient, NewTopic
        self.args = args
        broker = f"{args.host}:9092"
        if create:
            admin = AdminClient({"bootstrap.servers": broker})
            future = admin.create_topics([NewTopic(args.name, 1, 1,
                config={"flush.messages": "1", "min.insync.replicas": "1"})])[args.name]
            await asyncio.to_thread(future.result, 60)
        self.producer = Producer({"bootstrap.servers": broker, "acks": "all",
                                  "enable.idempotence": True, "linger.ms": 0})
        self.consumer = Consumer({"bootstrap.servers": broker, "group.id": args.name,
                                  "enable.auto.commit": False, "enable.auto.offset.store": False,
                                  "auto.offset.reset": "earliest", "fetch.wait.max.ms": 10})
        self.consumer.subscribe([args.name])

    async def produce(self, values):
        errors = []
        completed = []
        def delivered(error, message):
            (errors if error else completed).append(error if error else message.offset())
        for value in values:
            self.producer.produce(self.args.name, value.encode(), on_delivery=delivered)
        remaining = await asyncio.to_thread(self.producer.flush, 60)
        if remaining or errors or len(completed) != len(values):
            raise RuntimeError(f"Kafka delivery failed: {remaining}, {errors}")

    async def consume(self, count, empty=False):
        messages = []
        deadline = time.monotonic() + (5 if empty else 60)
        while len(messages) < count and time.monotonic() < deadline:
            batch = await asyncio.to_thread(self.consumer.consume, count - len(messages), 1)
            for message in batch:
                if message.error():
                    raise RuntimeError(str(message.error()))
            messages.extend(batch)
        if messages:
            self.consumer.store_offsets(message=messages[-1])
            await asyncio.to_thread(self.consumer.commit, asynchronous=False)
        return [message.value().decode() for message in messages]

    async def close(self):
        await asyncio.to_thread(self.consumer.close)


def summary(samples, operations, elapsed):
    ordered = sorted(samples)
    return {"operations": operations, "elapsed_seconds": elapsed,
            "operations_per_second": operations / elapsed,
            "batch_latency_ms": {label: ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]
                                 for label, fraction in [("p50", .5), ("p95", .95), ("p99", .99)]},
            "batch_samples_ms": samples}


async def run(args):
    adapter = {"reddb": RedQueue, "rabbitmq": RabbitQueue, "kafka": KafkaQueue}[args.engine]()
    await adapter.open(args, create=args.mode in ("run", "seed"))
    try:
        if args.mode in ("run", "seed") and args.settle_ms:
            # Explicit established-topology profile, outside timing. Keep the
            # zero-delay crash profile separately; never overwrite its failures.
            await asyncio.sleep(args.settle_ms / 1000)
        if args.mode == "empty":
            values = await adapter.consume(1, empty=True)
            if values:
                raise ValueError("acknowledged messages reappeared after restart")
            return {"verified_empty": True}
        # Warmup is untimed, including Kafka group assignment.
        if args.mode == "run":
            await adapter.produce([payload(args.operations, args.bytes)])
            validate(await adapter.consume(1), [args.operations], args.bytes)
        samples = []
        publication = []
        delivery = []
        started = time.perf_counter()
        for first in range(0, args.operations, args.batch):
            indices = list(range(first, min(first + args.batch, args.operations)))
            batch_started = time.perf_counter()
            if args.mode != "recover":
                await adapter.produce([payload(index, args.bytes) for index in indices])
            published = time.perf_counter()
            if args.mode != "seed":
                values = await adapter.consume(len(indices))
                # Concurrent producers may reorder a batch, never its membership.
                validate(values, indices, args.bytes)
            finished = time.perf_counter()
            publication.append((published - batch_started) * 1000)
            delivery.append((finished - published) * 1000)
            samples.append((finished - batch_started) * 1000)
        return {**summary(samples, args.operations, time.perf_counter() - started),
                "publication_batch_ms": publication, "consume_ack_batch_ms": delivery}
    finally:
        await adapter.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", choices=["reddb", "rabbitmq", "kafka"], required=True)
    parser.add_argument("--red-ack-batch", action="store_true", help="use RedDB atomic multi-message ACK")
    parser.add_argument("--host", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--batch", type=int, choices=[1, 16], default=1)
    parser.add_argument("--operations", type=int, default=256)
    parser.add_argument("--bytes", type=int, default=100)
    parser.add_argument("--settle-ms", type=int, default=0)
    parser.add_argument("--mode", choices=["run", "seed", "recover", "empty"], default="run")
    args = parser.parse_args()
    if args.operations < 1 or args.bytes < 13 or not 0 <= args.settle_ms <= 10_000 or not args.name.replace("_", "").isalnum():
        parser.error("positive operations, >=13 bytes and an alphanumeric queue name are required")
    result = {"config": vars(args), "status": "invalid", "classification": "shared-host-diagnostic",
              "latency_contract": "whole batch through consume/ACK; never divided by batch size",
              "ack_contract": {"reddb": "durable response", "rabbitmq": "one-way consumer ACK plus ordered RPC",
                               "kafka": "synchronous offset commit; records retained"}[args.engine]}
    try:
        result.update(asyncio.run(run(args)), status="valid-diagnostic")
    finally:
        print(json.dumps(result))


if __name__ == "__main__":
    main()
