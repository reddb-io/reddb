<?php
/** Inbound frame had COMPRESSED set but ext-zstd isn't loaded / failed to init. */

declare(strict_types=1);

namespace Reddb\RedDBException;

class CompressedButNoZstd extends ProtocolError
{
}
