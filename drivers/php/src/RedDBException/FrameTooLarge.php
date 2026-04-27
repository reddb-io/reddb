<?php
/** Frame length out of range (negative, < 16, or > 16 MiB). */

declare(strict_types=1);

namespace Reddb\RedDBException;

class FrameTooLarge extends ProtocolError
{
}
