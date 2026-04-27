// Package reddb provides a Go driver for RedDB that speaks RedWire (binary
// TCP) and the HTTP REST surface. Errors are typed so callers can switch on
// them without parsing strings.
package reddb

import (
	"errors"
	"fmt"
)

// ErrorCode classifies driver-level failures so application code can react
// without inspecting message text.
type ErrorCode string

const (
	// CodeUnsupportedScheme — URI scheme isn't one this driver understands.
	CodeUnsupportedScheme ErrorCode = "UNSUPPORTED_SCHEME"
	// CodeUnsupportedProto — `proto=` query parameter unknown.
	CodeUnsupportedProto ErrorCode = "UNSUPPORTED_PROTO"
	// CodeUnparseableURI — URL parser refused the input.
	CodeUnparseableURI ErrorCode = "UNPARSEABLE_URI"
	// CodeEmbeddedUnsupported — embedded (file/in-memory) modes aren't built in
	// for the pure-Go driver yet (cgo binding TBD).
	CodeEmbeddedUnsupported ErrorCode = "EMBEDDED_UNSUPPORTED"
	// CodeNetwork — TCP / TLS / DNS error.
	CodeNetwork ErrorCode = "NETWORK"
	// CodeProtocol — wire protocol violation (bad frame, missing field, etc).
	CodeProtocol ErrorCode = "PROTOCOL"
	// CodeAuthRefused — handshake / login rejected the credentials.
	CodeAuthRefused ErrorCode = "AUTH_REFUSED"
	// CodeEngine — server returned an error envelope.
	CodeEngine ErrorCode = "ENGINE"
	// CodeFrameTooLarge — encoded frame > MAX_FRAME_SIZE.
	CodeFrameTooLarge ErrorCode = "FRAME_TOO_LARGE"
	// CodeCompressedButNoZstd — peer sent a COMPRESSED frame but the local zstd
	// codec isn't initialised.
	CodeCompressedButNoZstd ErrorCode = "COMPRESSED_BUT_NO_ZSTD"
	// CodeNotFound — server replied 404 / `Get` came back empty.
	CodeNotFound ErrorCode = "NOT_FOUND"
	// CodeClosed — operation attempted on a closed connection.
	CodeClosed ErrorCode = "CLOSED"
)

// Error is the typed error returned across the driver surface.
type Error struct {
	Code    ErrorCode
	Message string
	Wrapped error
}

func (e *Error) Error() string {
	if e.Wrapped != nil {
		return fmt.Sprintf("reddb: %s: %s: %v", e.Code, e.Message, e.Wrapped)
	}
	return fmt.Sprintf("reddb: %s: %s", e.Code, e.Message)
}

func (e *Error) Unwrap() error { return e.Wrapped }

// NewError constructs a typed error.
func NewError(code ErrorCode, msg string) *Error {
	return &Error{Code: code, Message: msg}
}

// WrapError pairs a wrapped cause with a typed code.
func WrapError(code ErrorCode, msg string, err error) *Error {
	return &Error{Code: code, Message: msg, Wrapped: err}
}

// IsCode reports whether err (or any error it wraps) carries the given code.
func IsCode(err error, code ErrorCode) bool {
	var e *Error
	if errors.As(err, &e) {
		return e.Code == code
	}
	return false
}
