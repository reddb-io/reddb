package redwire

import (
	"errors"
	"sync"

	"github.com/klauspost/compress/zstd"
)

// zstd codec — pooled encoder + decoder so the framing layer stays sync. We
// init lazily; if a peer sends a COMPRESSED frame and init failed for any
// reason the decoder returns ErrCompressedButNoZstd.

var (
	zstdInitOnce sync.Once
	zstdEncoder  *zstd.Encoder
	zstdDecoder  *zstd.Decoder
	zstdInitErr  error
)

// ErrCompressedButNoZstd is returned when a COMPRESSED frame arrives but the
// local zstd codec failed to initialise.
var ErrCompressedButNoZstd = errors.New("redwire: COMPRESSED frame but zstd codec unavailable")

func ensureZstd() error {
	zstdInitOnce.Do(func() {
		// Encoder: level 1 by default. Matches the engine's default in
		// src/wire/redwire/codec.rs which honours RED_REDWIRE_ZSTD_LEVEL.
		enc, err := zstd.NewWriter(nil, zstd.WithEncoderLevel(zstd.SpeedFastest))
		if err != nil {
			zstdInitErr = err
			return
		}
		dec, err := zstd.NewReader(nil)
		if err != nil {
			_ = enc.Close()
			zstdInitErr = err
			return
		}
		zstdEncoder = enc
		zstdDecoder = dec
	})
	return zstdInitErr
}

func compressZstd(plain []byte) ([]byte, error) {
	if err := ensureZstd(); err != nil {
		return nil, err
	}
	return zstdEncoder.EncodeAll(plain, nil), nil
}

func decompressZstd(compressed []byte) ([]byte, error) {
	if err := ensureZstd(); err != nil {
		return nil, ErrCompressedButNoZstd
	}
	return zstdDecoder.DecodeAll(compressed, nil)
}
