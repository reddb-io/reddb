package dev.reddb

import dev.reddb.redwire.Flags
import dev.reddb.redwire.Frame
import dev.reddb.redwire.MessageKind
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder

class FrameTest {

    @Test
    fun roundTripEmptyPayload() {
        val f = Frame(MessageKind.Ping, 0, 0, 1L, ByteArray(0))
        val bytes = Frame.encode(f)
        assertEquals(Frame.HEADER_SIZE, bytes.size)
        val back = Frame.decode(bytes)
        assertEquals(f.kind, back.kind)
        assertEquals(f.streamId, back.streamId)
        assertEquals(f.correlationId, back.correlationId)
        assertEquals(0, back.payload.size)
    }

    @Test
    fun roundTripWithPayloadAndStream() {
        val body = "SELECT 1".toByteArray()
        val f = Frame(MessageKind.Query, 0, 7, 42L, body)
        val back = Frame.decode(Frame.encode(f))
        assertEquals(MessageKind.Query, back.kind)
        assertEquals(7, back.streamId)
        assertEquals(42L, back.correlationId)
        assertArrayEquals(body, back.payload)
    }

    @Test
    fun encodeWritesLittleEndianHeader() {
        val body = byteArrayOf(1, 2, 3)
        val f = Frame(MessageKind.Query, 0, 0, 0x0102030405060708L, body)
        val bytes = Frame.encode(f)
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        assertEquals(Frame.HEADER_SIZE + body.size, buf.int)
        assertEquals(MessageKind.Query, buf.get().toInt() and 0xff)
        assertEquals(0, buf.get().toInt() and 0xff)
        assertEquals(0, buf.short.toInt() and 0xffff)
        assertEquals(0x0102030405060708L, buf.long)
    }

    @Test
    fun decodeRejectsTruncatedHeader() {
        assertThrows(RedDBException.ProtocolError::class.java) { Frame.decode(ByteArray(5)) }
        assertThrows(RedDBException.ProtocolError::class.java) { Frame.decode(ByteArray(0)) }
    }

    @Test
    fun decodeRejectsLengthBelowHeader() {
        val bytes = ByteArray(Frame.HEADER_SIZE)
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, 15)
        assertThrows(RedDBException.FrameTooLarge::class.java) { Frame.decode(bytes) }
    }

    @Test
    fun decodeRejectsLengthAboveMax() {
        val bytes = ByteArray(Frame.HEADER_SIZE)
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, Frame.MAX_FRAME_SIZE + 1)
        assertThrows(RedDBException.FrameTooLarge::class.java) { Frame.decode(bytes) }
    }

    @Test
    fun decodeRejectsUnknownFlagBits() {
        val bytes = ByteArray(Frame.HEADER_SIZE)
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, Frame.HEADER_SIZE)
        bytes[4] = MessageKind.Ping.toByte()
        bytes[5] = 0b1000_0000.toByte()
        assertThrows(RedDBException.UnknownFlags::class.java) { Frame.decode(bytes) }
    }

    @Test
    fun decodeRejectsTruncatedPayload() {
        val bytes = ByteArray(20)
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, 32)
        bytes[4] = MessageKind.Query.toByte()
        assertThrows(RedDBException.ProtocolError::class.java) { Frame.decode(bytes) }
    }

    @Test
    fun encodeRefusesPayloadAboveMax() {
        val huge = ByteArray(Frame.MAX_FRAME_SIZE) // header + huge > max
        val f = Frame(MessageKind.Query, 1L, huge)
        assertThrows(RedDBException.FrameTooLarge::class.java) { Frame.encode(f) }
    }

    @Test
    fun encodedLengthReadsLengthPrefix() {
        val f = Frame(MessageKind.Result, 0, 0, 5L, byteArrayOf(1, 2, 3))
        val bytes = Frame.encode(f)
        assertEquals(bytes.size, Frame.encodedLength(bytes))
    }

    @Test
    fun compressedRoundTripRecoversPlaintext() {
        // Highly compressible — `abc` × 100.
        val plain = ByteArray(3 * 100) { i -> "abc"[i % 3].code.toByte() }
        val f = Frame(MessageKind.Result, Flags.COMPRESSED, 0, 7L, plain)
        val bytes = Frame.encode(f)
        // Wire form should be smaller than plaintext frame would be.
        assertTrue(
            bytes.size < Frame.HEADER_SIZE + plain.size,
            "compressed wire size ${bytes.size} >= plaintext ${Frame.HEADER_SIZE + plain.size}"
        )
        val back = Frame.decode(bytes)
        assertEquals(MessageKind.Result, back.kind)
        assertTrue(back.compressed())
        assertArrayEquals(plain, back.payload)
    }

    @Test
    fun uncompressedFrameDecodesUnchanged() {
        val plain = "hello world".toByteArray()
        val f = Frame(MessageKind.Result, 1L, plain)
        val back = Frame.decode(Frame.encode(f))
        assertArrayEquals(plain, back.payload)
        assertFalse(back.compressed())
    }

    @Test
    fun backToBackFramesDecodeIndependently() {
        val f1 = Frame(MessageKind.Query, 1L, "a".toByteArray())
        val f2 = Frame(MessageKind.Query, 2L, "bb".toByteArray())
        val a = Frame.encode(f1)
        val b = Frame.encode(f2)
        val both = ByteArray(a.size + b.size)
        System.arraycopy(a, 0, both, 0, a.size)
        System.arraycopy(b, 0, both, a.size, b.size)
        val firstLen = Frame.encodedLength(both)
        val back1 = Frame.decode(both.copyOfRange(0, firstLen))
        val back2 = Frame.decode(both.copyOfRange(firstLen, both.size))
        assertArrayEquals("a".toByteArray(), back1.payload)
        assertArrayEquals("bb".toByteArray(), back2.payload)
    }
}
