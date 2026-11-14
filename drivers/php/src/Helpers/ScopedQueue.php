<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final readonly class ScopedQueue
{
    public function __construct(private Queue $queue, public string $name) {}

    /** @param array{priority?:int} $opts */
    public function push(mixed $payload, array $opts = []): QueuePushResult
    {
        return $this->queue->push($this->name, $payload, $opts);
    }

    /** @return list<mixed> */
    public function pop(?int $count = null): array { return $this->queue->pop($this->name, $count); }
    /** @return list<mixed> */
    public function peek(?int $count = null): array { return $this->queue->peek($this->name, $count); }
    public function len(): int { return $this->queue->len($this->name); }
    public function purge(): DeleteResult { return $this->queue->purge($this->name); }

    /**
     * @param array{waitMs:int, group?:string, count?:int} $opts
     * @return list<QueueMessage>
     */
    public function readWait(string $consumer, array $opts): array
    {
        return $this->queue->readWaitEntries($this->name, $consumer, $opts);
    }

    public function ack(string $deliveryId): void { $this->queue->ack($this->name, $deliveryId); }
    public function nack(string $deliveryId, ?string $delay = null): void
    {
        $this->queue->nack($this->name, $deliveryId, $delay);
    }

    /**
     * @param array{consumer:string, waitMs?:int, group?:string, retryDelay?:string,
     *   onError?:(callable(\Throwable,?QueueMessage):void)} $opts
     * @param callable(QueueMessage):void $handler
     */
    public function consumer(callable $handler, array $opts): QueueConsumer
    {
        return new QueueConsumer($this, $handler, $opts);
    }
}
