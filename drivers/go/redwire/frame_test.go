package redwire

import (
	"bytes"
	"encoding/binary"
	"testing"
)

func roundTrip(t *testing.T, f *Frame) {
	t.Helper()
	enc, err := EncodeFrame(f)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	got, n, err := DecodeFrame(enc)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if n != len(enc) {
		t.Fatalf("consumed %d, encoded %d", n, len(enc))
	}
	if got.Kind != f.Kind {
		t.Errorf("kind: got 0x%02x want 0x%02x", got.Kind, f.Kind)
	}
	if got.CorrelationID != f.CorrelationID {
		t.Errorf("corr: got %d want %d", got.CorrelationID, f.CorrelationID)
	}
	if got.StreamID != f.StreamID {
		t.Errorf("stream: got %d want %d", got.StreamID, f.StreamID)
	}
	if !bytes.Equal(got.Payload, f.Payload) {
		t.Errorf("payload mismatch: got %q want %q", got.Payload, f.Payload)
	}
}

func TestEncodeDecode_EmptyPayload(t *testing.T) {
	roundTrip(t, NewFrame(KindPing, 1, nil))
}

func TestEncodeDecode_SmallPayload(t *testing.T) {
	roundTrip(t, NewFrame(KindQuery, 42, []byte("SELECT 1")))
}

func TestEncodeDecode_StreamID(t *testing.T) {
	f := NewFrame(KindResult, 7, []byte("hello"))
	f.StreamID = 9
	roundTrip(t, f)
}

func TestEncodeDecode_CompressedRoundTrip(t *testing.T) {
	// highly compressible payload
	payload := bytes.Repeat([]byte("abcabcabc"), 200)
	f := &Frame{
		Kind:          KindResult,
		CorrelationID: 5,
		Flags:         FlagCompressed,
		Payload:       payload,
	}
	enc, err := EncodeFrame(f)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if len(enc) >= FrameHeaderSize+len(payload) {
		t.Fatalf("compressed frame %d not smaller than plaintext %d",
			len(enc), FrameHeaderSize+len(payload))
	}
	got, _, err := DecodeFrame(enc)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !bytes.Equal(got.Payload, payload) {
		t.Errorf("payload mismatch after zstd round-trip")
	}
	if got.Flags&FlagCompressed == 0 {
		t.Errorf("decoded frame should retain COMPRESSED flag")
	}
}

func TestDecode_TruncatedHeader(t *testing.T) {
	if _, _, err := DecodeFrame([]byte{}); err == nil {
		t.Errorf("empty buf must error")
	}
	if _, _, err := DecodeFrame(make([]byte, 15)); err == nil {
		t.Errorf("15-byte buf must error")
	}
}

func TestDecode_LengthBelowHeader(t *testing.T) {
	buf := make([]byte, FrameHeaderSize)
	binary.LittleEndian.PutUint32(buf[0:4], 15) // < FrameHeaderSize
	if _, _, err := DecodeFrame(buf); err == nil {
		t.Errorf("expected error for length<16")
	}
}

func TestDecode_LengthOverMax(t *testing.T) {
	buf := make([]byte, FrameHeaderSize)
	binary.LittleEndian.PutUint32(buf[0:4], MaxFrameSize+1)
	if _, _, err := DecodeFrame(buf); err == nil {
		t.Errorf("expected error for length>max")
	}
}

func TestDecode_UnknownKind(t *testing.T) {
	buf := make([]byte, FrameHeaderSize)
	binary.LittleEndian.PutUint32(buf[0:4], FrameHeaderSize)
	buf[4] = 0xff
	if _, _, err := DecodeFrame(buf); err == nil {
		t.Errorf("expected error for unknown kind")
	}
}

func TestDecode_UnknownFlagBits(t *testing.T) {
	buf := make([]byte, FrameHeaderSize)
	binary.LittleEndian.PutUint32(buf[0:4], FrameHeaderSize)
	buf[4] = byte(KindPing)
	buf[5] = 0b1000_0000
	if _, _, err := DecodeFrame(buf); err == nil {
		t.Errorf("expected error for unknown flag bits")
	}
}

func TestEncode_RejectsUnknownFlags(t *testing.T) {
	f := &Frame{
		Kind:    KindPing,
		Flags:   0x80, // unknown
		Payload: nil,
	}
	if _, err := EncodeFrame(f); err == nil {
		t.Errorf("expected error encoding unknown flags")
	}
}

func TestDecode_PayloadTruncated(t *testing.T) {
	// header says length=20 but buffer has only 18.
	buf := make([]byte, 18)
	binary.LittleEndian.PutUint32(buf[0:4], 20)
	buf[4] = byte(KindPing)
	if _, _, err := DecodeFrame(buf); err == nil {
		t.Errorf("expected truncated-payload error")
	}
}

func TestDecode_TwoFramesBackToBack(t *testing.T) {
	a, _ := EncodeFrame(NewFrame(KindQuery, 1, []byte("a")))
	b, _ := EncodeFrame(NewFrame(KindQuery, 2, []byte("b")))
	stream := append(a, b...)

	g1, n1, err := DecodeFrame(stream)
	if err != nil {
		t.Fatalf("decode 1: %v", err)
	}
	g2, _, err := DecodeFrame(stream[n1:])
	if err != nil {
		t.Fatalf("decode 2: %v", err)
	}
	if !bytes.Equal(g1.Payload, []byte("a")) || !bytes.Equal(g2.Payload, []byte("b")) {
		t.Errorf("payloads got %q / %q", g1.Payload, g2.Payload)
	}
}
