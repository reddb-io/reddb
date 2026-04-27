<?php
/** Server refused the auth handshake (anonymous blocked, bad token, bad SCRAM proof, ...). */

declare(strict_types=1);

namespace Reddb\RedDBException;

use Reddb\RedDBException;

class AuthRefused extends RedDBException
{
}
