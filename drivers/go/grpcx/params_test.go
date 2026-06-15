package grpcx

import (
	"encoding/hex"
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"testing"

	pb "github.com/reddb-io/reddb-go/grpcx/proto"
	"github.com/reddb-io/reddb-go/redwire"
	"google.golang.org/protobuf/proto"
)

type paramsManifest struct {
	Values  []paramsValueFixture `json:"values"`
	Queries []paramsQueryFixture `json:"queries"`
}

type paramsValueFixture struct {
	Name    string `json:"name"`
	GrpcHex string `json:"grpc_hex"`
}

type paramsQueryFixture struct {
	Name           string   `json:"name"`
	SQL            string   `json:"sql"`
	Params         []string `json:"params"`
	GrpcRequestHex string   `json:"grpc_request_hex"`
}

func TestEncodeParamsMatchesSharedFixtures(t *testing.T) {
	manifest := loadParamsManifest(t)

	for _, fixture := range manifest.Values {
		encoded, err := EncodeParams([]any{grpcFixtureValue(t, fixture.Name)})
		if err != nil {
			t.Fatalf("%s: EncodeParams: %v", fixture.Name, err)
		}
		got := mustMarshalHex(t, encoded[0])
		if got != fixture.GrpcHex {
			t.Fatalf("%s: grpc hex mismatch\nwant %s\n got %s", fixture.Name, fixture.GrpcHex, got)
		}
	}

	for _, fixture := range manifest.Queries {
		params := make([]any, len(fixture.Params))
		for i, name := range fixture.Params {
			params[i] = grpcFixtureValue(t, name)
		}
		encoded, err := EncodeParams(params)
		if err != nil {
			t.Fatalf("%s: EncodeParams: %v", fixture.Name, err)
		}
		request := &pb.QueryRequest{Query: fixture.SQL, Params: encoded}
		got := mustMarshalHex(t, request)
		if got != fixture.GrpcRequestHex {
			t.Fatalf("%s: grpc request hex mismatch\nwant %s\n got %s", fixture.Name, fixture.GrpcRequestHex, got)
		}
	}
}

func loadParamsManifest(t *testing.T) paramsManifest {
	t.Helper()
	for _, path := range []string{
		filepath.Join("..", "..", "..", "testdata", "conformance", "redwire", "params", "manifest.json"),
		filepath.Join("..", "..", "testdata", "conformance", "redwire", "params", "manifest.json"),
	} {
		body, err := os.ReadFile(path)
		if err != nil {
			continue
		}
		var manifest paramsManifest
		if err := json.Unmarshal(body, &manifest); err != nil {
			t.Fatalf("manifest json: %v", err)
		}
		return manifest
	}
	t.Fatal("parameter fixture manifest not found")
	return paramsManifest{}
}

func grpcFixtureValue(t *testing.T, name string) any {
	t.Helper()
	switch name {
	case "null":
		return nil
	case "bool_true":
		return true
	case "bool_false":
		return false
	case "int_min":
		return int64(math.MinInt64)
	case "int_max":
		return int64(math.MaxInt64)
	case "int_42":
		return int64(42)
	case "float_nan":
		return math.Float64frombits(0x7ff8000000000000)
	case "float_pos_inf":
		return math.Inf(1)
	case "float_neg_inf":
		return math.Inf(-1)
	case "float_subnormal_min":
		return math.Float64frombits(1)
	case "text_unicode":
		return "h\u00e9llo"
	case "text_x":
		return "x"
	case "bytes_empty":
		return []byte{}
	case "bytes_deadbeef":
		return []byte{0xde, 0xad, 0xbe, 0xef}
	case "bytes_256":
		return bytes256()
	case "json_nested":
		return json.RawMessage(`{"a":null,"z":[1,{"deep":[true,false]}]}`)
	case "timestamp_zero":
		return redwire.Timestamp(0)
	case "timestamp_max":
		return redwire.Timestamp(math.MaxInt64)
	case "uuid_001122":
		uuid, err := redwire.UUIDFromString("00112233-4455-6677-8899-aabbccddeeff")
		if err != nil {
			t.Fatal(err)
		}
		return uuid
	case "vector_empty":
		return []float32{}
	case "vector_three":
		return []float32{1.0, 2.0, -0.5}
	case "vector_128":
		return vector128()
	default:
		t.Fatalf("unknown fixture %s", name)
		return nil
	}
}

func bytes256() []byte {
	out := make([]byte, 256)
	for i := range out {
		out[i] = byte(i)
	}
	return out
}

func vector128() []float32 {
	out := make([]float32, 128)
	for i := range out {
		out[i] = float32(i)
	}
	return out
}

func mustMarshalHex(t *testing.T, message proto.Message) string {
	t.Helper()
	body, err := proto.MarshalOptions{Deterministic: true}.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	return hex.EncodeToString(body)
}
