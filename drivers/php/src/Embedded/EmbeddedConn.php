<?php

declare(strict_types=1);

namespace Reddb\Embedded;

use Reddb\Conn;
use Reddb\RedDBException\EngineError;
use Reddb\RedDBException\EmbeddedUnsupported;
use Reddb\RedDBException\ProtocolError;

/** PHP facade over the versioned RedDB embedded C ABI. */
final class EmbeddedConn implements Conn
{
    private const ABI_VERSION = 1;

    private static ?\FFI $loadedFfi = null;

    /** @var \FFI\CData|null */
    private $handle;

    /** @param \FFI\CData $handle */
    private function __construct(private readonly \FFI $ffi, $handle)
    {
        $this->handle = $handle;
    }

    public static function open(?string $path): self
    {
        $ffi = self::nativeApi();
        $handle = $ffi->new('ReddbHandle *');
        $error = $ffi->new('char *');
        $status = $ffi->reddb_open($path ?? '', \FFI::addr($handle), \FFI::addr($error));
        $message = self::takeString($ffi, $error);
        if ($status !== 0) {
            throw new EngineError($message !== '' ? $message : "reddb_open failed with status {$status}");
        }
        if (\FFI::isNull($handle)) {
            throw new ProtocolError('reddb_open returned a null handle');
        }
        return new self($ffi, $handle);
    }

    private static function nativeApi(): \FFI
    {
        if (self::$loadedFfi !== null) {
            return self::$loadedFfi;
        }
        if (!extension_loaded('FFI')) {
            throw new EmbeddedUnsupported('embedded connections require the PHP FFI extension');
        }
        $library = getenv('REDDB_FFI_LIBRARY');
        if ($library === false || $library === '') {
            $library = match (PHP_OS_FAMILY) {
                'Windows' => 'reddb_ffi.dll',
                'Darwin' => 'libreddb_ffi.dylib',
                default => 'libreddb_ffi.so',
            };
        }
        try {
            $ffi = \FFI::cdef(<<<'CDEF'
                typedef unsigned int uint32_t;
                typedef int int32_t;
                typedef struct ReddbHandle ReddbHandle;
                uint32_t reddb_abi_version(void);
                int32_t reddb_open(const char *path, ReddbHandle **out_handle, char **out_error);
                int32_t reddb_invoke(ReddbHandle *handle, const char *method, const char *params_json,
                                     char **out_result, char **out_error);
                int32_t reddb_close(ReddbHandle *handle, char **out_error);
                void reddb_string_free(char *value);
                CDEF, $library);
        } catch (\Throwable $e) {
            throw new EmbeddedUnsupported('embedded native library unavailable: ' . $e->getMessage(), 0, $e);
        }
        $version = $ffi->reddb_abi_version();
        if ($version !== self::ABI_VERSION) {
            throw new ProtocolError(
                "incompatible RedDB embedded ABI: expected " . self::ABI_VERSION . ", found {$version}"
            );
        }
        self::$loadedFfi = $ffi;
        return $ffi;
    }

    /** @param \FFI\CData $pointer */
    private static function takeString(\FFI $ffi, $pointer): string
    {
        if (\FFI::isNull($pointer)) {
            return '';
        }
        try {
            return \FFI::string($pointer);
        } finally {
            $ffi->reddb_string_free($pointer);
        }
    }

    /** @param array<string,mixed> $params */
    private function invoke(string $method, array $params): string
    {
        if ($this->handle === null) {
            throw new \LogicException('embedded connection is closed');
        }
        $encoded = json_encode($params, JSON_THROW_ON_ERROR);
        $result = $this->ffi->new('char *');
        $error = $this->ffi->new('char *');
        $status = $this->ffi->reddb_invoke(
            $this->handle,
            $method,
            $encoded,
            \FFI::addr($result),
            \FFI::addr($error),
        );
        $message = self::takeString($this->ffi, $error);
        if ($status !== 0) {
            if (!\FFI::isNull($result)) {
                $this->ffi->reddb_string_free($result);
            }
            throw new EngineError($message !== '' ? $message : "reddb_invoke failed with status {$status}");
        }
        if (\FFI::isNull($result)) {
            throw new ProtocolError('reddb_invoke returned a null result');
        }
        return self::takeString($this->ffi, $result);
    }

    public function query(string $sql, array $params = []): string
    {
        $body = ['sql' => $sql];
        if ($params !== []) {
            $body['params'] = array_values($params);
        }
        return $this->invoke('query', $body);
    }

    public function insert(string $collection, array|object $payload): void
    {
        $this->invoke('insert', ['collection' => $collection, 'payload' => $payload]);
    }

    public function bulkInsert(string $collection, iterable $rows): void
    {
        $payloads = is_array($rows) ? array_values($rows) : iterator_to_array($rows, false);
        $this->invoke('bulk_insert', ['collection' => $collection, 'payloads' => $payloads]);
    }

    public function get(string $collection, string $id): string
    {
        return $this->invoke('get', ['collection' => $collection, 'id' => $id]);
    }

    public function delete(string $collection, string $id): void
    {
        $this->invoke('delete', ['collection' => $collection, 'id' => $id]);
    }

    public function ping(): void
    {
        $this->invoke('health', []);
    }

    public function close(): void
    {
        if ($this->handle === null) {
            return;
        }
        $handle = $this->handle;
        $this->handle = null;
        $error = $this->ffi->new('char *');
        $status = $this->ffi->reddb_close($handle, \FFI::addr($error));
        $message = self::takeString($this->ffi, $error);
        if ($status !== 0) {
            throw new EngineError($message !== '' ? $message : "reddb_close failed with status {$status}");
        }
    }
}
