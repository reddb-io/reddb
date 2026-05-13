package redwire

import (
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"sort"
	"time"
)

// ValueTag mirrors `reddb_wire::value::ValueTag`. Numeric values are part of
// the wire spec.
type ValueTag uint8

const (
	TagNull      ValueTag = 0x00
	TagBool      ValueTag = 0x01
	TagInt       ValueTag = 0x02
	TagFloat     ValueTag = 0x03
	TagText      ValueTag = 0x04
	TagBytes     ValueTag = 0x05
	TagVector    ValueTag = 0x06
	TagJSON      ValueTag = 0x07
	TagTimestamp ValueTag = 0x08
	TagUUID      ValueTag = 0x09
)

// MaxParamCount caps the params slice size on QueryWithParams. Mirrors the
// Rust + JS codecs.
const MaxParamCount = 65_536

// MaxValuePayloadLen caps a single length-prefixed value's bytes. Mirrors the
// frame-level cap so a misbehaving caller can't slip through with a single
// 100 MiB Json blob.
const MaxValuePayloadLen = int(MaxFrameSize)

// ErrUnsupportedParam is returned by EncodeValue when the Go value can't be
// coerced into a wire Value.
var ErrUnsupportedParam = errors.New("redwire: unsupported param type")

// UUID is the typed wrapper callers pass when they want a parameter encoded
// with `TagUUID` rather than as Bytes/Text.
type UUID [16]byte

// Timestamp is the typed wrapper callers pass when they need the full i64
// timestamp domain. time.Time remains the ergonomic mapping for normal dates.
type Timestamp int64

// UUIDFromString parses a canonical RFC 4122 hyphenated UUID. Hyphens
// elsewhere are tolerated; case is folded.
func UUIDFromString(s string) (UUID, error) {
	var out UUID
	hex := make([]byte, 0, 32)
	for i := 0; i < len(s); i++ {
		if s[i] == '-' {
			continue
		}
		hex = append(hex, s[i])
	}
	if len(hex) != 32 {
		return out, fmt.Errorf("redwire: uuid: bad length: %q", s)
	}
	for i := 0; i < 16; i++ {
		hi, err := hexNibble(hex[2*i])
		if err != nil {
			return out, fmt.Errorf("redwire: uuid: %w", err)
		}
		lo, err := hexNibble(hex[2*i+1])
		if err != nil {
			return out, fmt.Errorf("redwire: uuid: %w", err)
		}
		out[i] = (hi << 4) | lo
	}
	return out, nil
}

// EncodeValue serialises one parameter for the `QueryWithParams` payload.
// Mirrors `reddb_wire::value::encode` and `drivers/js/src/redwire.js`'s
// `encodeValue`.
//
// Native Go type mapping:
//
//	nil                            -> Null
//	bool                           -> Bool
//	int / int8..64 / uint8..uint32 -> Int (i64)
//	uint / uint64 (<= MaxInt64)    -> Int (i64)
//	float32, float64               -> Float (f64)
//	string                         -> Text
//	[]byte                         -> Bytes
//	[]float32 / []float64          -> Vector (f32)
//	time.Time                      -> Timestamp (unix seconds)
//	UUID                           -> Uuid
//	json.RawMessage / map / []any  -> Json (canonical bytes)
//	*T                             -> recurse (nil pointer -> Null)
func EncodeValue(v any) ([]byte, error) {
	switch x := v.(type) {
	case nil:
		return []byte{byte(TagNull)}, nil
	case bool:
		var b byte
		if x {
			b = 1
		}
		return []byte{byte(TagBool), b}, nil
	case int:
		return encodeInt(int64(x)), nil
	case int8:
		return encodeInt(int64(x)), nil
	case int16:
		return encodeInt(int64(x)), nil
	case int32:
		return encodeInt(int64(x)), nil
	case int64:
		return encodeInt(x), nil
	case uint:
		if uint64(x) > math.MaxInt64 {
			return nil, fmt.Errorf("redwire: uint param %d > i64 max", x)
		}
		return encodeInt(int64(x)), nil
	case uint8:
		return encodeInt(int64(x)), nil
	case uint16:
		return encodeInt(int64(x)), nil
	case uint32:
		return encodeInt(int64(x)), nil
	case uint64:
		if x > math.MaxInt64 {
			return nil, fmt.Errorf("redwire: uint64 param %d > i64 max", x)
		}
		return encodeInt(int64(x)), nil
	case float32:
		return encodeFloat(float64(x)), nil
	case float64:
		return encodeFloat(x), nil
	case string:
		return encodeLenPrefixed(TagText, []byte(x))
	case []byte:
		return encodeLenPrefixed(TagBytes, x)
	case []float32:
		return encodeVector(x)
	case []float64:
		f32 := make([]float32, len(x))
		for i, f := range x {
			f32[i] = float32(f)
		}
		return encodeVector(f32)
	case time.Time:
		return encodeTimestamp(x.Unix()), nil
	case Timestamp:
		return encodeTimestamp(int64(x)), nil
	case UUID:
		out := make([]byte, 1+16)
		out[0] = byte(TagUUID)
		copy(out[1:], x[:])
		return out, nil
	case json.RawMessage:
		var anyv any
		if err := json.Unmarshal(x, &anyv); err != nil {
			return nil, fmt.Errorf("redwire: json.RawMessage param: %w", err)
		}
		body, err := canonicalJSON(anyv)
		if err != nil {
			return nil, err
		}
		return encodeLenPrefixed(TagJSON, body)
	case map[string]any:
		body, err := canonicalJSON(x)
		if err != nil {
			return nil, err
		}
		return encodeLenPrefixed(TagJSON, body)
	case []any:
		body, err := canonicalJSON(x)
		if err != nil {
			return nil, err
		}
		return encodeLenPrefixed(TagJSON, body)
	}

	if isNilPointer(v) {
		return []byte{byte(TagNull)}, nil
	}
	if rv := derefPointer(v); rv != nil {
		return EncodeValue(rv)
	}
	return nil, fmt.Errorf("%w: %T", ErrUnsupportedParam, v)
}

func encodeInt(x int64) []byte {
	out := make([]byte, 1+8)
	out[0] = byte(TagInt)
	binary.LittleEndian.PutUint64(out[1:], uint64(x))
	return out
}

func encodeFloat(x float64) []byte {
	out := make([]byte, 1+8)
	out[0] = byte(TagFloat)
	binary.LittleEndian.PutUint64(out[1:], math.Float64bits(x))
	return out
}

func encodeTimestamp(secs int64) []byte {
	out := make([]byte, 1+8)
	out[0] = byte(TagTimestamp)
	binary.LittleEndian.PutUint64(out[1:], uint64(secs))
	return out
}

func encodeLenPrefixed(tag ValueTag, bytes []byte) ([]byte, error) {
	if len(bytes) > MaxValuePayloadLen {
		return nil, fmt.Errorf("redwire: value len %d > MaxValuePayloadLen %d",
			len(bytes), MaxValuePayloadLen)
	}
	out := make([]byte, 1+4+len(bytes))
	out[0] = byte(tag)
	binary.LittleEndian.PutUint32(out[1:5], uint32(len(bytes)))
	copy(out[5:], bytes)
	return out, nil
}

func encodeVector(f32 []float32) ([]byte, error) {
	bytes := len(f32) * 4
	if bytes > MaxValuePayloadLen {
		return nil, fmt.Errorf("redwire: vector bytes %d > MaxValuePayloadLen %d",
			bytes, MaxValuePayloadLen)
	}
	out := make([]byte, 1+4+bytes)
	out[0] = byte(TagVector)
	binary.LittleEndian.PutUint32(out[1:5], uint32(len(f32)))
	for i, f := range f32 {
		binary.LittleEndian.PutUint32(out[5+i*4:5+(i+1)*4], math.Float32bits(f))
	}
	return out, nil
}

// canonicalJSON serialises v with byte-for-byte parity against the server's
// canonical JSON (sorted object keys, no extra whitespace). Mirrors
// `crate::json::canonical` and the JS `canonicalJson`.
func canonicalJSON(v any) ([]byte, error) {
	return appendCanonical(nil, v)
}

func appendCanonical(dst []byte, v any) ([]byte, error) {
	switch x := v.(type) {
	case nil:
		return append(dst, "null"...), nil
	case bool:
		if x {
			return append(dst, "true"...), nil
		}
		return append(dst, "false"...), nil
	case float64:
		if math.IsNaN(x) || math.IsInf(x, 0) {
			return append(dst, "null"...), nil
		}
		b, err := json.Marshal(x)
		if err != nil {
			return nil, err
		}
		return append(dst, b...), nil
	case json.Number:
		return append(dst, x.String()...), nil
	case string:
		b, err := json.Marshal(x)
		if err != nil {
			return nil, err
		}
		return append(dst, b...), nil
	case []any:
		dst = append(dst, '[')
		for i, item := range x {
			if i > 0 {
				dst = append(dst, ',')
			}
			var err error
			dst, err = appendCanonical(dst, item)
			if err != nil {
				return nil, err
			}
		}
		return append(dst, ']'), nil
	case map[string]any:
		keys := make([]string, 0, len(x))
		for k := range x {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		dst = append(dst, '{')
		for i, k := range keys {
			if i > 0 {
				dst = append(dst, ',')
			}
			kb, err := json.Marshal(k)
			if err != nil {
				return nil, err
			}
			dst = append(dst, kb...)
			dst = append(dst, ':')
			dst, err = appendCanonical(dst, x[k])
			if err != nil {
				return nil, err
			}
		}
		return append(dst, '}'), nil
	}
	return nil, fmt.Errorf("redwire: canonicalJSON: unsupported type %T", v)
}

// EncodeQueryWithParams builds the `QueryWithParams` payload body. Layout:
//
//	u32 sql_len LE | utf-8 sql | u32 param_count LE | N encoded values
func EncodeQueryWithParams(sql string, params []any) ([]byte, error) {
	if len(params) > MaxParamCount {
		return nil, fmt.Errorf("redwire: param_count %d > MaxParamCount %d",
			len(params), MaxParamCount)
	}
	sqlBytes := []byte(sql)
	if len(sqlBytes) > MaxValuePayloadLen {
		return nil, fmt.Errorf("redwire: sql_len %d > MaxValuePayloadLen %d",
			len(sqlBytes), MaxValuePayloadLen)
	}
	encoded := make([][]byte, len(params))
	total := 4 + len(sqlBytes) + 4
	for i, p := range params {
		b, err := EncodeValue(p)
		if err != nil {
			return nil, fmt.Errorf("redwire: param[%d]: %w", i, err)
		}
		encoded[i] = b
		total += len(b)
	}
	buf := make([]byte, total)
	pos := 0
	binary.LittleEndian.PutUint32(buf[pos:pos+4], uint32(len(sqlBytes)))
	pos += 4
	copy(buf[pos:pos+len(sqlBytes)], sqlBytes)
	pos += len(sqlBytes)
	binary.LittleEndian.PutUint32(buf[pos:pos+4], uint32(len(encoded)))
	pos += 4
	for _, b := range encoded {
		copy(buf[pos:pos+len(b)], b)
		pos += len(b)
	}
	return buf, nil
}

func derefPointer(v any) any {
	switch p := v.(type) {
	case *bool:
		return *p
	case *int:
		return *p
	case *int64:
		return *p
	case *float64:
		return *p
	case *string:
		return *p
	case *[]byte:
		return *p
	case *time.Time:
		return *p
	case *UUID:
		return *p
	}
	return nil
}

func isNilPointer(v any) bool {
	switch p := v.(type) {
	case *bool:
		return p == nil
	case *int:
		return p == nil
	case *int64:
		return p == nil
	case *float64:
		return p == nil
	case *string:
		return p == nil
	case *[]byte:
		return p == nil
	case *time.Time:
		return p == nil
	case *UUID:
		return p == nil
	}
	return false
}
