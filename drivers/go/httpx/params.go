package httpx

import (
	"encoding/base64"
	"fmt"
	"math"
	"strconv"
	"time"
)

// UUID mirrors `redwire.UUID` for the HTTP transport. Kept here to avoid an
// httpx → redwire dependency cycle; callers passing UUIDs go through the
// top-level driver facade which accepts `redwire.UUID` and converts.
type UUID [16]byte

// paramToJSON converts one Go parameter to its JSON shape per ADR 0011.
// Native JSON types pass through; non-native types use envelope objects:
//   - []byte                -> {"$bytes": <base64>}
//   - time.Time             -> {"$ts": <unix seconds>}
//   - UUID                  -> {"$uuid": "xxxxxxxx-..."}
//   - []float32 / []float64 -> JSON number array (Vector)
//
// Pointers follow their pointee; nil pointers become JSON null.
func paramToJSON(v any) (any, error) {
	switch x := v.(type) {
	case nil:
		return nil, nil
	case bool, string,
		int, int8, int16, int32, int64,
		float32, float64:
		return x, nil
	case uint, uint8, uint16, uint32:
		return x, nil
	case uint64:
		if x > uint64(1<<63-1) {
			return map[string]any{"$uint": strconv.FormatUint(x, 10)}, nil
		}
		return x, nil
	case []byte:
		return map[string]any{"$bytes": base64.StdEncoding.EncodeToString(x)}, nil
	case []float32:
		out := make([]any, len(x))
		for i, f := range x {
			if math.IsNaN(float64(f)) || math.IsInf(float64(f), 0) {
				return nil, fmt.Errorf("vector contains non-finite value at index %d", i)
			}
			out[i] = float64(f)
		}
		return out, nil
	case []float64:
		out := make([]any, len(x))
		for i, f := range x {
			if math.IsNaN(f) || math.IsInf(f, 0) {
				return nil, fmt.Errorf("vector contains non-finite value at index %d", i)
			}
			out[i] = f
		}
		return out, nil
	case time.Time:
		return map[string]any{"$ts": x.Unix()}, nil
	case UUID:
		return map[string]any{"$uuid": formatUUID(x)}, nil
	case map[string]any, []any:
		return x, nil
	}
	if rv := derefPointer(v); rv != nil {
		return paramToJSON(rv)
	}
	if isNilPointer(v) {
		return nil, nil
	}
	return nil, fmt.Errorf("unsupported param type %T", v)
}

func formatUUID(u UUID) string {
	const hex = "0123456789abcdef"
	// Layout: 8-4-4-4-12 = 36 chars incl. hyphens.
	out := make([]byte, 36)
	pos := 0
	groups := []int{4, 2, 2, 2, 6}
	idx := 0
	for gi, g := range groups {
		for j := 0; j < g; j++ {
			b := u[idx]
			out[pos] = hex[b>>4]
			out[pos+1] = hex[b&0x0F]
			pos += 2
			idx++
		}
		if gi < len(groups)-1 {
			out[pos] = '-'
			pos++
		}
	}
	return string(out)
}

func derefPointer(v any) any {
	switch p := v.(type) {
	case *bool:
		if p == nil {
			return nil
		}
		return *p
	case *int:
		if p == nil {
			return nil
		}
		return *p
	case *int64:
		if p == nil {
			return nil
		}
		return *p
	case *float64:
		if p == nil {
			return nil
		}
		return *p
	case *string:
		if p == nil {
			return nil
		}
		return *p
	case *[]byte:
		if p == nil {
			return nil
		}
		return *p
	case *time.Time:
		if p == nil {
			return nil
		}
		return *p
	case *UUID:
		if p == nil {
			return nil
		}
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
