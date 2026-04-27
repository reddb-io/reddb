<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\RedDBException\FrameTooLarge;
use Reddb\RedDBException\ProtocolError;
use Reddb\RedDBException\UnknownFlags;
use Reddb\Redwire\Codec;
use Reddb\Redwire\Frame;

final class FrameTest extends TestCase
{
    public function test_round_trip_empty_payload(): void
    {
        $f = new Frame(Frame::KIND_PING, 0, 0, 1, '');
        $bytes = Frame::encode($f);
        $this->assertSame(Frame::HEADER_SIZE, strlen($bytes));
        $back = Frame::decode($bytes);
        $this->assertSame($f->kind, $back->kind);
        $this->assertSame($f->streamId, $back->streamId);
        $this->assertSame($f->correlationId, $back->correlationId);
        $this->assertSame('', $back->payload);
    }

    public function test_round_trip_with_payload_and_stream(): void
    {
        $body = 'SELECT 1';
        $f = new Frame(Frame::KIND_QUERY, 0, 7, 42, $body);
        $back = Frame::decode(Frame::encode($f));
        $this->assertSame(Frame::KIND_QUERY, $back->kind);
        $this->assertSame(7, $back->streamId);
        $this->assertSame(42, $back->correlationId);
        $this->assertSame($body, $back->payload);
    }

    public function test_encode_writes_little_endian_header(): void
    {
        $body = "\x01\x02\x03";
        $f = new Frame(Frame::KIND_QUERY, 0, 0, 0x0102030405060708, $body);
        $bytes = Frame::encode($f);
        $hdr = unpack('Vlength/Ckind/Cflags/vstream/Pcorr', $bytes);
        $this->assertSame(Frame::HEADER_SIZE + strlen($body), $hdr['length']);
        $this->assertSame(Frame::KIND_QUERY, $hdr['kind']);
        $this->assertSame(0, $hdr['flags']);
        $this->assertSame(0, $hdr['stream']);
        $this->assertSame(0x0102030405060708, $hdr['corr']);
    }

    public function test_decode_rejects_truncated_header(): void
    {
        $this->expectException(ProtocolError::class);
        Frame::decode(str_repeat("\x00", 5));
    }

    public function test_decode_rejects_empty_buffer(): void
    {
        $this->expectException(ProtocolError::class);
        Frame::decode('');
    }

    public function test_decode_rejects_length_below_header(): void
    {
        $bytes = pack('VCCvP', 15, Frame::KIND_PING, 0, 0, 0);
        $this->expectException(FrameTooLarge::class);
        Frame::decode($bytes);
    }

    public function test_decode_rejects_length_above_max(): void
    {
        $bytes = pack('VCCvP', Frame::MAX_FRAME_SIZE + 1, Frame::KIND_PING, 0, 0, 0);
        $this->expectException(FrameTooLarge::class);
        Frame::decode($bytes);
    }

    public function test_decode_rejects_unknown_flag_bits(): void
    {
        $bytes = pack('VCCvP', Frame::HEADER_SIZE, Frame::KIND_PING, 0b1000_0000, 0, 0);
        $this->expectException(UnknownFlags::class);
        Frame::decode($bytes);
    }

    public function test_decode_rejects_truncated_payload(): void
    {
        // length says 32 but we only supply 20 bytes.
        $bytes = pack('VCCvP', 32, Frame::KIND_QUERY, 0, 0, 0);
        $this->expectException(ProtocolError::class);
        Frame::decode($bytes);
    }

    public function test_encode_refuses_payload_above_max(): void
    {
        $huge = str_repeat("\x00", Frame::MAX_FRAME_SIZE);
        $this->expectException(FrameTooLarge::class);
        Frame::encode(Frame::make(Frame::KIND_QUERY, 1, $huge));
    }

    public function test_encoded_length_reads_length_prefix(): void
    {
        $f = new Frame(Frame::KIND_RESULT, 0, 0, 5, "\x01\x02\x03");
        $bytes = Frame::encode($f);
        $this->assertSame(strlen($bytes), Frame::encodedLength($bytes));
    }

    public function test_compressed_round_trip_recovers_plaintext(): void
    {
        if (!Codec::isAvailable()) {
            $this->markTestSkipped('ext-zstd is not loaded');
        }
        // Highly compressible — `abc` × 100.
        $plain = str_repeat('abc', 100);
        $f = new Frame(Frame::KIND_RESULT, Frame::FLAG_COMPRESSED, 0, 7, $plain);
        $bytes = Frame::encode($f);
        $this->assertLessThan(
            Frame::HEADER_SIZE + strlen($plain),
            strlen($bytes),
            'compressed wire size should be smaller than plaintext frame'
        );
        $back = Frame::decode($bytes);
        $this->assertSame(Frame::KIND_RESULT, $back->kind);
        $this->assertTrue($back->compressed());
        $this->assertSame($plain, $back->payload);
    }

    public function test_compressed_inbound_fails_gracefully_without_zstd(): void
    {
        if (Codec::isAvailable()) {
            $this->markTestSkipped('ext-zstd present — fallback path covered by other tests');
        }
        // Build a synthetic compressed frame: header sets COMPRESSED but
        // body is just opaque bytes. Decode must throw CompressedButNoZstd.
        $body = "\x28\xb5\x2f\xfd\x00"; // looks-like zstd magic, won't decode
        $bytes = pack('VCCvP', Frame::HEADER_SIZE + strlen($body), Frame::KIND_RESULT, Frame::FLAG_COMPRESSED, 0, 1) . $body;
        $this->expectException(\Reddb\RedDBException\CompressedButNoZstd::class);
        Frame::decode($bytes);
    }

    public function test_uncompressed_frame_decodes_unchanged(): void
    {
        $plain = 'hello world';
        $back = Frame::decode(Frame::encode(Frame::make(Frame::KIND_RESULT, 1, $plain)));
        $this->assertSame($plain, $back->payload);
        $this->assertFalse($back->compressed());
    }

    public function test_back_to_back_frames_decode_independently(): void
    {
        $f1 = Frame::make(Frame::KIND_QUERY, 1, 'a');
        $f2 = Frame::make(Frame::KIND_QUERY, 2, 'bb');
        $a = Frame::encode($f1);
        $b = Frame::encode($f2);
        $both = $a . $b;

        $firstLen = Frame::encodedLength($both);
        $back1 = Frame::decode(substr($both, 0, $firstLen));
        $back2 = Frame::decode(substr($both, $firstLen));
        $this->assertSame('a', $back1->payload);
        $this->assertSame(1, $back1->correlationId);
        $this->assertSame('bb', $back2->payload);
        $this->assertSame(2, $back2->correlationId);
    }

    public function test_known_flags_constant_matches_spec(): void
    {
        $this->assertSame(0b0000_0011, Frame::KNOWN_FLAGS);
    }

    public function test_kind_name_falls_back_to_hex_for_unknown(): void
    {
        $this->assertSame('Query', Frame::kindName(Frame::KIND_QUERY));
        $this->assertSame('0xff', Frame::kindName(0xff));
    }
}
