package redwire

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math"
)

const typedInsertVersion byte = 1

// Field is one named typed value in a TypedInsert request.
type Field struct {
	Name  string
	Value any
}

// ErrTypedMutationsUnsupported is returned before any frame is written when
// the server did not advertise FeatureTypedMutations.
var ErrTypedMutationsUnsupported = errors.New(
	"redwire: server did not advertise FEATURE_TYPED_MUTATIONS — upgrade the server",
)

// EncodeTypedInsert serialises a single row without SQL or a JSON request body.
func EncodeTypedInsert(collection string, fields []Field) ([]byte, error) {
	if len(collection) == 0 {
		return nil, errors.New("redwire: typed insert: collection cannot be empty")
	}
	if len(collection) > math.MaxUint16 {
		return nil, errors.New("redwire: typed insert: collection is too long")
	}
	if len(fields) > math.MaxUint16 {
		return nil, errors.New("redwire: typed insert: too many fields")
	}

	out := make([]byte, 0, 5+len(collection))
	out = append(out, typedInsertVersion)
	out = binary.LittleEndian.AppendUint16(out, uint16(len(collection)))
	out = append(out, collection...)
	out = binary.LittleEndian.AppendUint16(out, uint16(len(fields)))
	seen := make(map[string]struct{}, len(fields))
	for _, field := range fields {
		if len(field.Name) == 0 {
			return nil, errors.New("redwire: typed insert: field name cannot be empty")
		}
		if len(field.Name) > math.MaxUint16 {
			return nil, errors.New("redwire: typed insert: field name is too long")
		}
		if _, exists := seen[field.Name]; exists {
			return nil, fmt.Errorf("redwire: typed insert: duplicate field %q", field.Name)
		}
		seen[field.Name] = struct{}{}
		value, err := EncodeValue(field.Value)
		if err != nil {
			return nil, fmt.Errorf("redwire: typed insert: field %q: %w", field.Name, err)
		}
		out = binary.LittleEndian.AppendUint16(out, uint16(len(field.Name)))
		out = append(out, field.Name...)
		out = append(out, value...)
	}
	return out, nil
}
