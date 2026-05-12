package grpcx

import (
	"encoding/json"
	"fmt"
	"math"
	"time"

	pb "github.com/reddb-io/reddb-go/grpcx/proto"
	"github.com/reddb-io/reddb-go/redwire"
)

// EncodeParams converts Go bind values into the gRPC QueryValue oneof. It
// mirrors the public Go mapping used by RedWire and HTTP.
func EncodeParams(params []any) ([]*pb.QueryValue, error) {
	if len(params) == 0 {
		return nil, nil
	}
	out := make([]*pb.QueryValue, len(params))
	for i, p := range params {
		v, err := encodeParam(p)
		if err != nil {
			return nil, fmt.Errorf("param %d: %w", i+1, err)
		}
		out[i] = v
	}
	return out, nil
}

func encodeParam(v any) (*pb.QueryValue, error) {
	switch x := v.(type) {
	case nil:
		return &pb.QueryValue{Kind: &pb.QueryValue_NullValue{NullValue: &pb.QueryNull{}}}, nil
	case bool:
		return &pb.QueryValue{Kind: &pb.QueryValue_BoolValue{BoolValue: x}}, nil
	case int:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case int8:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case int16:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case int32:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case int64:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: x}}, nil
	case uint:
		if uint64(x) > math.MaxInt64 {
			return nil, fmt.Errorf("uint param %d > i64 max", x)
		}
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case uint8:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case uint16:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case uint32:
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case uint64:
		if x > math.MaxInt64 {
			return nil, fmt.Errorf("uint64 param %d > i64 max", x)
		}
		return &pb.QueryValue{Kind: &pb.QueryValue_IntValue{IntValue: int64(x)}}, nil
	case float32:
		return &pb.QueryValue{Kind: &pb.QueryValue_FloatValue{FloatValue: float64(x)}}, nil
	case float64:
		return &pb.QueryValue{Kind: &pb.QueryValue_FloatValue{FloatValue: x}}, nil
	case string:
		return &pb.QueryValue{Kind: &pb.QueryValue_TextValue{TextValue: x}}, nil
	case []byte:
		return &pb.QueryValue{Kind: &pb.QueryValue_BytesValue{BytesValue: x}}, nil
	case []float32:
		return &pb.QueryValue{Kind: &pb.QueryValue_VectorValue{VectorValue: &pb.QueryVector{Values: x}}}, nil
	case []float64:
		values := make([]float32, len(x))
		for i, f := range x {
			values[i] = float32(f)
		}
		return &pb.QueryValue{Kind: &pb.QueryValue_VectorValue{VectorValue: &pb.QueryVector{Values: values}}}, nil
	case time.Time:
		return &pb.QueryValue{Kind: &pb.QueryValue_TimestampValue{TimestampValue: x.Unix()}}, nil
	case redwire.UUID:
		bytes := make([]byte, len(x))
		copy(bytes, x[:])
		return &pb.QueryValue{Kind: &pb.QueryValue_UuidValue{UuidValue: bytes}}, nil
	case json.RawMessage:
		body, err := canonicalJSON(x)
		if err != nil {
			return nil, err
		}
		return &pb.QueryValue{Kind: &pb.QueryValue_JsonValue{JsonValue: string(body)}}, nil
	case map[string]any, []any:
		body, err := json.Marshal(x)
		if err != nil {
			return nil, err
		}
		return &pb.QueryValue{Kind: &pb.QueryValue_JsonValue{JsonValue: string(body)}}, nil
	}

	if isNilPointer(v) {
		return &pb.QueryValue{Kind: &pb.QueryValue_NullValue{NullValue: &pb.QueryNull{}}}, nil
	}
	if rv := derefPointer(v); rv != nil {
		return encodeParam(rv)
	}
	return nil, fmt.Errorf("unsupported param type %T", v)
}

func canonicalJSON(raw json.RawMessage) ([]byte, error) {
	var v any
	if err := json.Unmarshal(raw, &v); err != nil {
		return nil, fmt.Errorf("json.RawMessage param: %w", err)
	}
	return json.Marshal(v)
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
	case *redwire.UUID:
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
	case *redwire.UUID:
		return p == nil
	}
	return false
}
