<?php
/** Peer set a flag bit we don't recognise — bail out per the spec. */

declare(strict_types=1);

namespace Reddb\RedDBException;

class UnknownFlags extends ProtocolError
{
}
