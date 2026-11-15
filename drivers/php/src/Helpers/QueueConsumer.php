<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/** Managed at-least-once worker. Run it in a dedicated PHP process. */
final class QueueConsumer
{
    private bool $stopping = false;
    private bool $paused = false;
    private int $inFlight = 0;
    private string $state = 'idle';

    /**
     * PHP executes one handler per process. Run multiple worker processes for
     * parallelism. The optional context provides cooperative lease heartbeats.
     *
     * @param callable(QueueMessage,QueueHandlerContext):void $handler
     * @param array{consumer:string, waitMs?:int, group?:string, retryDelay?:string,
     *   concurrency?:int, leaseHeartbeatMs?:int, handlerTimeoutMs?:int,
     *   receiveBackoff?:array{initialMs?:int,maxMs?:int,multiplier?:float,jitter?:bool},
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
        if (!is_int($waitMs) || $waitMs <= 0) {
            throw new InvalidArgument('queue consumer waitMs must be greater than zero');
        }
        if (($opts['concurrency'] ?? 1) !== 1) {
            throw new InvalidArgument(
                'PHP queue consumers support concurrency=1; use multiple worker processes'
            );
        }
        foreach (['leaseHeartbeatMs', 'handlerTimeoutMs'] as $duration) {
            if (isset($opts[$duration]) && (!is_int($opts[$duration]) || $opts[$duration] < 0)) {
                throw new InvalidArgument("queue consumer {$duration} must be non-negative");
            }
        }
        $backoff = $opts['receiveBackoff'] ?? [];
        $initial = $backoff['initialMs'] ?? 250;
        $max = $backoff['maxMs'] ?? 30_000;
        $multiplier = $backoff['multiplier'] ?? 2.0;
        if (!is_int($initial) || $initial <= 0 || !is_int($max) || $max < $initial
            || !is_numeric($multiplier) || (float) $multiplier < 1.0) {
            throw new InvalidArgument('invalid queue receive backoff');
        }
    }

    /** Blocks the current process until stop is requested or an unhandled error occurs. */
    public function run(): void
    {
        if ($this->state !== 'idle') {
            throw new InvalidArgument('queue consumer already started');
        }
        $this->state = 'running';
        $read = [
            'waitMs' => $this->opts['waitMs'] ?? 30_000,
            'group' => $this->opts['group'] ?? null,
            'count' => 1,
        ];
        $receiveFailures = 0;
        try {
            while (!$this->stopping) {
                while ($this->paused && !$this->stopping) {
                    usleep(10_000);
                }
                if ($this->stopping) break;
                try {
                    $messages = $this->queue->readWait($this->opts['consumer'], $read);
                    $receiveFailures = 0;
                } catch (\Throwable $error) {
                    $this->report($error, null);
                    $receiveFailures++;
                    $this->backoff($receiveFailures);
                    continue;
                }
                if ($messages === []) continue;
                $this->process($messages[0]);
            }
            $this->state = 'stopped';
        } catch (\Throwable $error) {
            $this->state = 'failed';
            throw $error;
        }
    }

    public function stop(): void
    {
        $this->stopping = true;
        $this->paused = false;
        if (!in_array($this->state, ['idle', 'stopped', 'failed'], true)) {
            $this->state = 'stopping';
        }
    }

    public function pause(): void
    {
        if ($this->state === 'running') {
            $this->paused = true;
            $this->state = 'paused';
        }
    }

    public function resume(): void
    {
        if ($this->state === 'paused') {
            $this->paused = false;
            $this->state = 'running';
        }
    }

    /** @return array{state:string,isRunning:bool,isPaused:bool,inFlight:int,concurrency:int} */
    public function status(): array
    {
        return [
            'state' => $this->state,
            'isRunning' => in_array($this->state, ['running', 'paused'], true),
            'isPaused' => $this->paused,
            'inFlight' => $this->inFlight,
            'concurrency' => 1,
        ];
    }

    private function process(QueueMessage $message): void
    {
        if ($message->deliveryId === '') {
            $this->report(new InvalidResponse('queue delivery is missing delivery_id'), $message);
            return;
        }
        $heartbeatMs = $this->opts['leaseHeartbeatMs'] ?? 0;
        $context = new QueueHandlerContext($this->queue, $message->deliveryId, $heartbeatMs);
        $started = hrtime(true);
        $handlerError = null;
        $this->inFlight++;
        try {
            ($this->handler)($message, $context);
        } catch (\Throwable $error) {
            $handlerError = $error;
        } finally {
            $this->inFlight--;
        }
        if ($context->leaseLost()) {
            // Ownership is uncertain after EXTEND fails: never ACK or NACK.
            $this->report($handlerError ?? new \RuntimeException('queue lease lost'), $message);
            return;
        }
        $timeoutMs = $this->opts['handlerTimeoutMs'] ?? 0;
        $elapsedMs = (hrtime(true) - $started) / 1_000_000;
        if ($timeoutMs > 0 && $elapsedMs >= $timeoutMs) {
            $context->cancel();
            $this->queue->nack($message->deliveryId, $this->opts['retryDelay'] ?? null);
            $this->report(
                new QueueHandlerTimeout("queue handler exceeded {$timeoutMs}ms"),
                $message,
            );
            return;
        }
        if ($handlerError !== null) {
            $this->queue->nack($message->deliveryId, $this->opts['retryDelay'] ?? null);
            $this->report($handlerError, $message);
            return;
        }
        try {
            $this->queue->ack($message->deliveryId);
        } catch (\Throwable $error) {
            // An ambiguous ACK must never be followed by a NACK.
            $this->report($error, $message);
        }
    }

    private function backoff(int $failures): void
    {
        $options = $this->opts['receiveBackoff'] ?? [];
        $initial = $options['initialMs'] ?? 250;
        $max = $options['maxMs'] ?? 30_000;
        $multiplier = (float) ($options['multiplier'] ?? 2.0);
        $delay = min($max, $initial * ($multiplier ** ($failures - 1)));
        if ($options['jitter'] ?? true) {
            $delay *= random_int(0, 1_000_000) / 1_000_000;
        }
        usleep((int) $delay * 1000);
    }

    private function report(\Throwable $error, ?QueueMessage $message): void
    {
        $onError = $this->opts['onError'] ?? null;
        if ($onError === null) throw $error;
        $onError($error, $message);
    }
}
