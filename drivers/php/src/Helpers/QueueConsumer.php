<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/** Sequential at-least-once worker. Run it in a dedicated PHP process. */
final class QueueConsumer
{
    private bool $stopping = false;

    /**
     * @param callable(QueueMessage):void $handler
     * @param array{consumer:string, waitMs?:int, group?:string, retryDelay?:string,
     *   onError?:(callable(\Throwable,?QueueMessage):void)} $opts
     */
    public function __construct(
        private readonly ScopedQueue $queue,
        private readonly mixed $handler,
        private readonly array $opts,
    ) {
        $consumer = $opts['consumer'] ?? '';
        Sql::assertIdentifier($consumer, 'consumer name');
        if (isset($opts['group']) && $opts['group'] !== '') {
            Sql::assertIdentifier($opts['group'], 'group name');
        }
        $waitMs = $opts['waitMs'] ?? 30_000;
        if (!is_int($waitMs) || $waitMs < 0) {
            throw new InvalidArgument('queue consumer waitMs must be non-negative');
        }
    }

    /** Blocks the current process until stop is requested or an unhandled error occurs. */
    public function run(): void
    {
        $read = [
            'waitMs' => $this->opts['waitMs'] ?? 30_000,
            'group' => $this->opts['group'] ?? null,
            'count' => 1,
        ];
        while (!$this->stopping) {
            try {
                $messages = $this->queue->readWait($this->opts['consumer'], $read);
            } catch (\Throwable $error) {
                $this->report($error, null);
                continue;
            }
            if ($messages === []) continue;
            $message = $messages[0];
            try {
                if ($message->deliveryId === '') {
                    throw new InvalidResponse('queue delivery is missing delivery_id');
                }
                ($this->handler)($message);
            } catch (\Throwable $error) {
                if ($message->deliveryId !== '') {
                    $this->queue->nack($message->deliveryId, $this->opts['retryDelay'] ?? null);
                }
                $this->report($error, $message);
                continue;
            }
            try {
                $this->queue->ack($message->deliveryId);
            } catch (\Throwable $error) {
                $this->report($error, $message);
            }
        }
    }

    public function stop(): void { $this->stopping = true; }

    private function report(\Throwable $error, ?QueueMessage $message): void
    {
        $onError = $this->opts['onError'] ?? null;
        if ($onError === null) throw $error;
        $onError($error, $message);
    }
}
