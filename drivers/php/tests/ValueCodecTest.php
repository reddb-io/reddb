<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\Redwire\ValueCodec;
use Reddb\Value;

final class ValueCodecTest extends TestCase
{
    public function test_value_tag_table_is_pinned(): void
    {
        $this->assertSame(0x00, ValueCodec::TAG_NULL);
        $this->assertSame(0x01, ValueCodec::TAG_BOOL);
        $this->assertSame(0x02, ValueCodec::TAG_INT);
        $this->assertSame(0x03, ValueCodec::TAG_FLOAT);
        $this->assertSame(0x04, ValueCodec::TAG_TEXT);
        $this->assertSame(0x05, ValueCodec::TAG_BYTES);
        $this->assertSame(0x06, ValueCodec::TAG_VECTOR);
        $this->assertSame(0x07, ValueCodec::TAG_JSON);
        $this->assertSame(0x08, ValueCodec::TAG_TIMESTAMP);
        $this->assertSame(0x09, ValueCodec::TAG_UUID);
    }

    public function test_encode_scalar_values(): void
    {
        $this->assertSame("\x00", ValueCodec::encodeValue(null));
        $this->assertSame("\x01\x01", ValueCodec::encodeValue(true));
        $this->assertSame("\x01\x00", ValueCodec::encodeValue(false));
        $this->assertSame("\x02\x01\x00\x00\x00\x00\x00\x00\x00", ValueCodec::encodeValue(1));
        $this->assertSame("\x02" . str_repeat("\xff", 8), ValueCodec::encodeValue(-1));
        $this->assertSame("\x04\x01\x00\x00\x00x", ValueCodec::encodeValue('x'));
    }

    public function test_encode_bytes_timestamp_uuid_and_json(): void
    {
        $this->assertSame("\x05\x04\x00\x00\x00\xde\xad\xbe\xef", ValueCodec::encodeValue(Value::bytes("\xde\xad\xbe\xef")));

        $ts = ValueCodec::encodeValue(new \DateTimeImmutable('@1700000000'));
        $this->assertSame(ValueCodec::TAG_TIMESTAMP, ord($ts[0]));
        $this->assertSame(1700000000, unpack('P', substr($ts, 1, 8))[1]);

        $uuid = ValueCodec::encodeValue(Value::uuid('00112233-4455-6677-8899-aabbccddeeff'));
        $this->assertSame(ValueCodec::TAG_UUID, ord($uuid[0]));
        $this->assertSame(hex2bin('00112233445566778899aabbccddeeff'), substr($uuid, 1));

        $json = ValueCodec::encodeValue(Value::json(['b' => 2, 'a' => 1]));
        $this->assertSame("\x07\x0d\x00\x00\x00{\"a\":1,\"b\":2}", $json);
    }

    public function test_encode_vector_from_float_array(): void
    {
        $encoded = ValueCodec::encodeValue([1.0, 2.0, -0.5]);
        $this->assertSame(ValueCodec::TAG_VECTOR, ord($encoded[0]));
        $this->assertSame(3, unpack('V', substr($encoded, 1, 4))[1]);
        $this->assertSame(1.0, unpack('g', substr($encoded, 5, 4))[1]);
        $this->assertSame(2.0, unpack('g', substr($encoded, 9, 4))[1]);
        $this->assertSame(-0.5, unpack('g', substr($encoded, 13, 4))[1]);
    }

    public function test_encode_query_with_params_payload(): void
    {
        $encoded = ValueCodec::encodeQueryWithParams('Q', [42, 'x', null]);
        $this->assertSame([1], array_values(unpack('V', substr($encoded, 0, 4))));
        $this->assertSame('Q', $encoded[4]);
        $this->assertSame([3], array_values(unpack('V', substr($encoded, 5, 4))));
        $this->assertSame(ValueCodec::TAG_INT, ord($encoded[9]));
        $this->assertSame(42, unpack('P', substr($encoded, 10, 8))[1]);
        $this->assertSame(ValueCodec::TAG_TEXT, ord($encoded[18]));
        $this->assertSame("\x01\x00\x00\x00x", substr($encoded, 19, 5));
        $this->assertSame(ValueCodec::TAG_NULL, ord($encoded[24]));
        $this->assertSame(25, strlen($encoded));
    }

    public function test_http_params_use_json_envelopes_for_tagged_values(): void
    {
        $params = ValueCodec::toHttpParams([
            null,
            true,
            42,
            1.5,
            'txt',
            Value::bytes("hi"),
            [1.0, 2.0],
            Value::json(['b' => 2, 'a' => 1]),
            new \DateTimeImmutable('@1700000000'),
            Value::uuid('00112233-4455-6677-8899-aabbccddeeff'),
        ]);

        $this->assertSame([
            null,
            true,
            42,
            1.5,
            'txt',
            ['$bytes' => 'aGk='],
            [1.0, 2.0],
            ['a' => 1, 'b' => 2],
            ['$ts' => 1700000000],
            ['$uuid' => '00112233-4455-6677-8899-aabbccddeeff'],
        ], $params);
    }
}
