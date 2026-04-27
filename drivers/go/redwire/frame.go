// Package redwire encodes / decodes RedWire frames and runs the
// handshake state machine over a TCP / TLS stream. Mirrors
// drivers/rust/src/redwire/ and drivers/js/src/redwire.js — the wire
// shape is fixed by ADR 0001.
package redwire

import (
	"encoding/binary"
	"errors"
	"fmt"
)

// FrameHeaderSize is the fixed 16-byte header that prefixes every frame.
const FrameHeaderSize = 16

// MaxFrameSize is the largest legal encoded frame (header + on-wire payload).
// Anything larger is rejected at decode time so a malformed peer can't allocate
// arbitrary memory on the receiver.
const MaxFrameSize uint32 = 16 * 1024 * 1024

// KnownFlags is the bitmask of every flag the spec defines today. Decoders
// reject any frame that sets bits outside this mask so future flag additions
// fail loudly instead of silently passing.
const KnownFlags uint8 = 0b11

// Flag bits.
const (
	FlagCompressed uint8 = 1 << 0 // 0x01
	FlagMoreFrames uint8 = 1 << 1 // 0x02
)

// MessageKind is the single-byte kind discriminator. Numeric values are part of
// the wire spec — never repurpose a value once shipped.
type MessageKind uint8

// Message kinds. Mirrors src/wire/redwire/frame.rs.
const (
	KindQuery                  MessageKind = 0x01
	KindResult                 MessageKind = 0x02
	KindError                  MessageKind = 0x03
	KindBulkInsert             MessageKind = 0x04
	KindBulkOk                 MessageKind = 0x05
	KindBulkInsertBinary       MessageKind = 0x06
	KindQueryBinary            MessageKind = 0x07
	KindBulkInsertPrevalidated MessageKind = 0x08
	KindHello                  MessageKind = 0x10
	KindHelloAck               MessageKind = 0x11
	KindAuthRequest            MessageKind = 0x12
	KindAuthResponse           MessageKind = 0x13
	KindAuthOk                 MessageKind = 0x14
	KindAuthFail               MessageKind = 0x15
	KindBye                    MessageKind = 0x16
	KindPing                   MessageKind = 0x17
	KindPong                   MessageKind = 0x18
	KindGet                    MessageKind = 0x19
	KindDelete                 MessageKind = 0x1A
	KindDeleteOk               MessageKind = 0x1B
)

// FrameHeader captures the parsed 16-byte header.
type FrameHeader struct {
	Length        uint32 // total frame length incl. header
	Kind          MessageKind
	Flags         uint8
	StreamID      uint16
	CorrelationID uint64
}

// Frame is a decoded RedWire frame. Payload is always the plaintext body — the
// codec inflates compressed frames before delivery so callers never see zstd.
type Frame struct {
	Kind          MessageKind
	Flags         uint8
	StreamID      uint16
	CorrelationID uint64
	Payload       []byte
}

// EncodeFrame serialises the frame to its wire form. Compresses the payload
// when FlagCompressed is set; the COMPRESSED bit on the wire stays so the peer
// knows to decompress.
func EncodeFrame(f *Frame) ([]byte, error) {
	if f == nil {
		return nil, errors.New("redwire: encode nil frame")
	}
	if f.Flags&^KnownFlags != 0 {
		return nil, fmt.Errorf("redwire: encode: unknown flag bits 0x%02x", f.Flags)
	}

	onWire := f.Payload
	flags := f.Flags
	if flags&FlagCompressed != 0 {
		c, err := compressZstd(f.Payload)
		if err != nil {
			// Fallback: ship plaintext, drop the COMPRESSED flag so the peer
			// doesn't try to decompress.
			flags &^= FlagCompressed
		} else {
			onWire = c
		}
	}

	length := uint32(FrameHeaderSize + len(onWire))
	if length > MaxFrameSize {
		return nil, fmt.Errorf("redwire: encode: frame %d > MaxFrameSize %d",
			length, MaxFrameSize)
	}

	buf := make([]byte, length)
	binary.LittleEndian.PutUint32(buf[0:4], length)
	buf[4] = byte(f.Kind)
	buf[5] = flags
	binary.LittleEndian.PutUint16(buf[6:8], f.StreamID)
	binary.LittleEndian.PutUint64(buf[8:16], f.CorrelationID)
	copy(buf[FrameHeaderSize:], onWire)
	return buf, nil
}

// DecodeFrame parses a single frame from the start of buf. Returns the frame
// and the number of bytes consumed (always equal to header.Length on success).
// Compressed frames are inflated before return.
func DecodeFrame(buf []byte) (*Frame, int, error) {
	if len(buf) < FrameHeaderSize {
		return nil, 0, fmt.Errorf("redwire: decode: header truncated (%d < %d)",
			len(buf), FrameHeaderSize)
	}
	length := binary.LittleEndian.Uint32(buf[0:4])
	if length < FrameHeaderSize {
		return nil, 0, fmt.Errorf("redwire: decode: invalid length %d (< %d)",
			length, FrameHeaderSize)
	}
	if length > MaxFrameSize {
		return nil, 0, fmt.Errorf("redwire: decode: invalid length %d (> %d)",
			length, MaxFrameSize)
	}
	if uint32(len(buf)) < length {
		return nil, 0, fmt.Errorf(
			"redwire: decode: payload truncated, expected %d bytes, got %d",
			length, len(buf))
	}
	kindByte := buf[4]
	if !isKnownKind(MessageKind(kindByte)) {
		return nil, 0, fmt.Errorf("redwire: decode: unknown kind 0x%02x", kindByte)
	}
	flags := buf[5]
	if flags&^KnownFlags != 0 {
		return nil, 0, fmt.Errorf("redwire: decode: unknown flag bits 0x%02x", flags)
	}
	streamID := binary.LittleEndian.Uint16(buf[6:8])
	corr := binary.LittleEndian.Uint64(buf[8:16])

	onWire := buf[FrameHeaderSize:length]
	payload := onWire
	if flags&FlagCompressed != 0 {
		plain, err := decompressZstd(onWire)
		if err != nil {
			return nil, 0, fmt.Errorf("redwire: decode: zstd inflate: %w", err)
		}
		payload = plain
	} else {
		// Defensive copy — the input buffer is owned by the reader and gets
		// reused across calls.
		payload = append([]byte(nil), onWire...)
	}
	return &Frame{
		Kind:          MessageKind(kindByte),
		Flags:         flags,
		StreamID:      streamID,
		CorrelationID: corr,
		Payload:       payload,
	}, int(length), nil
}

// NewFrame builds a plaintext frame ready for EncodeFrame.
func NewFrame(kind MessageKind, corr uint64, payload []byte) *Frame {
	return &Frame{
		Kind:          kind,
		CorrelationID: corr,
		Payload:       payload,
	}
}

func isKnownKind(k MessageKind) bool {
	switch k {
	case KindQuery, KindResult, KindError,
		KindBulkInsert, KindBulkOk,
		KindBulkInsertBinary, KindQueryBinary, KindBulkInsertPrevalidated,
		KindHello, KindHelloAck,
		KindAuthRequest, KindAuthResponse, KindAuthOk, KindAuthFail,
		KindBye, KindPing, KindPong,
		KindGet, KindDelete, KindDeleteOk:
		return true
	}
	// Accept the control-plane numeric range that the engine reserves so we
	// don't reject frames the engine might mint (cancel/notice/etc.).
	if k >= 0x20 && k <= 0x27 {
		return true
	}
	return false
}
