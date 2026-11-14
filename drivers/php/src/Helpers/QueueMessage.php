<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final readonly class QueueMessage
{
    /** @param array<string,mixed> $raw */
    public function __construct(
        public string $messageId,
        public string $deliveryId,
        public mixed $payload,
        public ?string $consumer,
        public int $deliveryCount,
        public array $raw,
    ) {}
}
