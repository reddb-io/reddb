package dev.reddb;

import dev.reddb.redwire.Frame;
import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Arrays;

import static org.junit.jupiter.api.Assertions.*;

class FrameTest {

    @Test
    void roundTripEmptyPayload() {
        Frame f = new Frame(Frame.Kind.Ping, 0, 0, 1L, new byte[0]);
        byte[] bytes = Frame.encode(f);
        assertEquals(Frame.HEADER_SIZE, bytes.length);
        Frame back = Frame.decode(bytes);
        assertEquals(f.kind, back.kind);
        assertEquals(f.streamId, back.streamId);
        assertEquals(f.correlationId, back.correlationId);
        assertEquals(0, back.payload.length);
    }

    @Test
    void roundTripWithPayloadAndStream() {
        byte[] body = "SELECT 1".getBytes();
        Frame f = new Frame(Frame.Kind.Query, 0, 7, 42L, body);
        Frame back = Frame.decode(Frame.encode(f));
        assertEquals(Frame.Kind.Query, back.kind);
        assertEquals(7, back.streamId);
        assertEquals(42L, back.correlationId);
        assertArrayEquals(body, back.payload);
    }

    @Test
    void encodeWritesLittleEndianHeader() {
        byte[] body = new byte[]{1, 2, 3};
        Frame f = new Frame(Frame.Kind.Query, 0, 0, 0x0102030405060708L, body);
        byte[] bytes = Frame.encode(f);
        ByteBuffer buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN);
        assertEquals(Frame.HEADER_SIZE + body.length, buf.getInt());
        assertEquals(Frame.Kind.Query, buf.get() & 0xff);
        assertEquals(0, buf.get() & 0xff);
        assertEquals(0, buf.getShort() & 0xffff);
        assertEquals(0x0102030405060708L, buf.getLong());
    }

    @Test
    void decodeRejectsTruncatedHeader() {
        assertThrows(RedDBException.ProtocolError.class, () -> Frame.decode(new byte[5]));
        assertThrows(RedDBException.ProtocolError.class, () -> Frame.decode(new byte[0]));
    }

    @Test
    void decodeRejectsLengthBelowHeader() {
        byte[] bytes = new byte[Frame.HEADER_SIZE];
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, 15);
        assertThrows(RedDBException.FrameTooLarge.class, () -> Frame.decode(bytes));
    }

    @Test
    void decodeRejectsLengthAboveMax() {
        byte[] bytes = new byte[Frame.HEADER_SIZE];
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, Frame.MAX_FRAME_SIZE + 1);
        assertThrows(RedDBException.FrameTooLarge.class, () -> Frame.decode(bytes));
    }

    @Test
    void decodeRejectsUnknownFlagBits() {
        byte[] bytes = new byte[Frame.HEADER_SIZE];
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, Frame.HEADER_SIZE);
        bytes[4] = (byte) Frame.Kind.Ping;
        bytes[5] = (byte) 0b1000_0000;
        assertThrows(RedDBException.UnknownFlags.class, () -> Frame.decode(bytes));
    }

    @Test
    void decodeRejectsTruncatedPayload() {
        // length says 32 but we only supply 20 bytes.
        byte[] bytes = new byte[20];
        ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).putInt(0, 32);
        bytes[4] = (byte) Frame.Kind.Query;
        assertThrows(RedDBException.ProtocolError.class, () -> Frame.decode(bytes));
    }

    @Test
    void encodeRefusesPayloadAboveMax() {
        byte[] huge = new byte[Frame.MAX_FRAME_SIZE]; // header + huge > max
        Frame f = new Frame(Frame.Kind.Query, 1L, huge);
        assertThrows(RedDBException.FrameTooLarge.class, () -> Frame.encode(f));
    }

    @Test
    void encodedLengthReadsLengthPrefix() {
        Frame f = new Frame(Frame.Kind.Result, 0, 0, 5L, new byte[]{1, 2, 3});
        byte[] bytes = Frame.encode(f);
        assertEquals(bytes.length, Frame.encodedLength(bytes));
    }

    @Test
    void compressedRoundTripRecoversPlaintext() {
        // Highly compressible — `abc` × 100.
        byte[] plain = new byte[3 * 100];
        for (int i = 0; i < plain.length; i++) plain[i] = (byte) ("abc".charAt(i % 3));
        Frame f = new Frame(Frame.Kind.Result, Frame.Flags.COMPRESSED, 0, 7L, plain);
        byte[] bytes = Frame.encode(f);
        // Wire form should be smaller than the plaintext frame would have been.
        assertTrue(bytes.length < Frame.HEADER_SIZE + plain.length,
            "compressed wire size " + bytes.length + " >= plaintext " + (Frame.HEADER_SIZE + plain.length));
        Frame back = Frame.decode(bytes);
        assertEquals(Frame.Kind.Result, back.kind);
        assertTrue(back.compressed());
        assertArrayEquals(plain, back.payload);
    }

    @Test
    void uncompressedFrameDecodesUnchanged() {
        byte[] plain = "hello world".getBytes();
        Frame f = new Frame(Frame.Kind.Result, 1L, plain);
        Frame back = Frame.decode(Frame.encode(f));
        assertArrayEquals(plain, back.payload);
        assertFalse(back.compressed());
    }

    @Test
    void backToBackFramesDecodeIndependently() {
        Frame f1 = new Frame(Frame.Kind.Query, 1L, "a".getBytes());
        Frame f2 = new Frame(Frame.Kind.Query, 2L, "bb".getBytes());
        byte[] a = Frame.encode(f1);
        byte[] b = Frame.encode(f2);
        byte[] both = new byte[a.length + b.length];
        System.arraycopy(a, 0, both, 0, a.length);
        System.arraycopy(b, 0, both, a.length, b.length);
        // Decode the first by slicing on its declared length.
        int firstLen = Frame.encodedLength(both);
        Frame back1 = Frame.decode(Arrays.copyOfRange(both, 0, firstLen));
        Frame back2 = Frame.decode(Arrays.copyOfRange(both, firstLen, both.length));
        assertArrayEquals("a".getBytes(), back1.payload);
        assertArrayEquals("bb".getBytes(), back2.payload);
    }
}
