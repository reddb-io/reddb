<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/** Cooperative controls for synchronous PHP queue handlers. */
final class QueueHandlerContext
{
    private bool $cancelled = false;
    private bool $leaseLost = false;
    private float $lastHeartbeat;

    public function __construct(
        private readonly ScopedQueue $queue,
        private readonly string $deliveryId,
        private readonly int $heartbeatMs,
    ) {
        $this->lastHeartbeat = microtime(true);
    }

    public function isCancelled(): bool { return $this->cancelled; }
    public function leaseLost(): bool { return $this->leaseLost; }
    public function cancel(): void { $this->cancelled = true; }

    public function extendLease(): void
    {
        try {
            $this->queue->extend($this->deliveryId);
            $this->lastHeartbeat = microtime(true);
        } catch (\Throwable $error) {
            $this->leaseLost = true;
            $this->cancelled = true;
            throw $error;
        }
    }

    /** Extend the lease when its configured heartbeat interval has elapsed. */
    public function checkpoint(): void
    {
        if ($this->heartbeatMs > 0
            && (microtime(true) - $this->lastHeartbeat) * 1000 >= $this->heartbeatMs) {
            $this->extendLease();
        }
    }

    /** Cooperative sleep that keeps the delivery lease alive. */
    public function sleep(int $milliseconds): void
    {
        $remaining = $milliseconds;
        while ($remaining > 0 && !$this->cancelled) {
            $slice = $this->heartbeatMs > 0
                ? min($remaining, max(1, intdiv($this->heartbeatMs, 2)))
                : $remaining;
            usleep($slice * 1000);
            $remaining -= $slice;
            $this->checkpoint();
        }
    }
}
