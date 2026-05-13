package redwire

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestEncodeValue_Null(t *testing.T) {
	got, err := EncodeValue(nil)
	if err != nil {
		t.Fatalf("encode nil: %v", err)
	}
	if !bytes.Equal(got, []byte{0x00}) {
		t.Errorf("nil encoding: got % x", got)
	}
}

func TestEncodeValue_Bool(t *testing.T) {
	cases := []struct {
		in   bool
		want []byte
	}{
		{true, []byte{0x01, 0x01}},
		{false, []byte{0x01, 0x00}},
	}
	for _, c := range cases {
		got, err := EncodeValue(c.in)
		if err != nil {
			t.Fatalf("encode %v: %v", c.in, err)
		}
		if !bytes.Equal(got, c.want) {
			t.Errorf("bool %v: got % x want % x", c.in, got, c.want)
		}
	}
}

func TestEncodeValue_Int(t *testing.T) {
	cases := []any{
		int(-1), int8(-1), int16(-1), int32(-1), int64(-1),
		uint(1), uint8(1), uint16(1), uint32(1), uint64(1),
	}
	want := []byte{byte(TagInt)}
	for i, v := range cases {
		got, err := EncodeValue(v)
		if err != nil {
			t.Fatalf("case %d (%T): %v", i, v, err)
		}
		if got[0] != want[0] {
			t.Errorf("case %d tag: got %#x want %#x", i, got[0], want[0])
		}
		if len(got) != 9 {
			t.Errorf("case %d length: got %d want 9", i, len(got))
		}
	}
}

func TestEncodeValue_Int64Wire(t *testing.T) {
	got, err := EncodeValue(int64(-2))
	if err != nil {
		t.Fatal(err)
	}
	want := []byte{byte(TagInt), 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff}
	if !bytes.Equal(got, want) {
		t.Errorf("got % x want % x", got, want)
	}
}

func TestEncodeValue_Uint64Overflow(t *testing.T) {
	_, err := EncodeValue(uint64(math.MaxUint64))
	if err == nil {
		t.Fatal("expected error on uint64 > MaxInt64")
	}
}

func TestEncodeValue_Float(t *testing.T) {
	got, err := EncodeValue(float64(3.14))
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagFloat) {
		t.Errorf("tag: got %#x", got[0])
	}
	bits := binary.LittleEndian.Uint64(got[1:9])
	if math.Float64frombits(bits) != 3.14 {
		t.Errorf("roundtrip: got %v", math.Float64frombits(bits))
	}

	// float32 also lands on Float tag.
	got32, err := EncodeValue(float32(1.5))
	if err != nil {
		t.Fatal(err)
	}
	if got32[0] != byte(TagFloat) {
		t.Errorf("float32 tag: got %#x", got32[0])
	}
}

func TestEncodeValue_Text(t *testing.T) {
	got, err := EncodeValue("hi")
	if err != nil {
		t.Fatal(err)
	}
	want := []byte{byte(TagText), 0x02, 0x00, 0x00, 0x00, 'h', 'i'}
	if !bytes.Equal(got, want) {
		t.Errorf("got % x want % x", got, want)
	}
}

func TestEncodeValue_TextUnicode(t *testing.T) {
	got, err := EncodeValue("héllo🦀")
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagText) {
		t.Fatal("tag")
	}
	declared := int(binary.LittleEndian.Uint32(got[1:5]))
	if declared != len(got)-5 {
		t.Errorf("len mismatch: header=%d payload=%d", declared, len(got)-5)
	}
}

func TestEncodeValue_Bytes(t *testing.T) {
	got, err := EncodeValue([]byte{0xde, 0xad})
	if err != nil {
		t.Fatal(err)
	}
	want := []byte{byte(TagBytes), 0x02, 0x00, 0x00, 0x00, 0xde, 0xad}
	if !bytes.Equal(got, want) {
		t.Errorf("got % x want % x", got, want)
	}
}

func TestEncodeValue_VectorF32(t *testing.T) {
	got, err := EncodeValue([]float32{1.0, 2.0})
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagVector) {
		t.Fatal("tag")
	}
	n := binary.LittleEndian.Uint32(got[1:5])
	if n != 2 {
		t.Errorf("len: %d", n)
	}
	v0 := math.Float32frombits(binary.LittleEndian.Uint32(got[5:9]))
	v1 := math.Float32frombits(binary.LittleEndian.Uint32(got[9:13]))
	if v0 != 1.0 || v1 != 2.0 {
		t.Errorf("values: %v %v", v0, v1)
	}
}

func TestEncodeValue_VectorF64Downcasts(t *testing.T) {
	got, err := EncodeValue([]float64{1.5, -2.5})
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagVector) {
		t.Fatal("tag")
	}
	if binary.LittleEndian.Uint32(got[1:5]) != 2 {
		t.Errorf("count")
	}
}

func TestEncodeValue_Timestamp(t *testing.T) {
	tm := time.Unix(1_700_000_000, 999) // sub-second part dropped
	got, err := EncodeValue(tm)
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagTimestamp) {
		t.Fatal("tag")
	}
	secs := int64(binary.LittleEndian.Uint64(got[1:9]))
	if secs != 1_700_000_000 {
		t.Errorf("secs: %d", secs)
	}
}

func TestEncodeValue_UUID(t *testing.T) {
	u, err := UUIDFromString("550e8400-e29b-41d4-a716-446655440000")
	if err != nil {
		t.Fatal(err)
	}
	got, err := EncodeValue(u)
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagUUID) {
		t.Fatal("tag")
	}
	if len(got) != 17 {
		t.Errorf("len: %d", len(got))
	}
}

func TestUUIDFromString_BadInputs(t *testing.T) {
	cases := []string{
		"",
		"not-a-uuid",
		"550e8400-e29b-41d4-a716-44665544", // too short
		"550e8400-e29b-41d4-a716-446655440000-extra", // too long
		"550e8400-e29b-41d4-a716-44665544000Z",       // non-hex char
	}
	for _, c := range cases {
		if _, err := UUIDFromString(c); err == nil {
			t.Errorf("expected error for %q", c)
		}
	}
}

func TestEncodeValue_Json_Canonical(t *testing.T) {
	got, err := EncodeValue(map[string]any{"b": 1.0, "a": "x"})
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagJSON) {
		t.Fatal("tag")
	}
	bodyLen := binary.LittleEndian.Uint32(got[1:5])
	body := got[5 : 5+bodyLen]
	// keys sorted alphabetically: a then b
	if string(body) != `{"a":"x","b":1}` {
		t.Errorf("canonical body: %q", body)
	}
}

func TestEncodeValue_PointerFollowNil(t *testing.T) {
	var p *string
	got, err := EncodeValue(p)
	if err != nil {
		t.Fatalf("encode nil *string: %v", err)
	}
	if !bytes.Equal(got, []byte{0x00}) {
		t.Errorf("nil *string: % x", got)
	}
}

func TestEncodeValue_PointerFollow(t *testing.T) {
	s := "hi"
	got, err := EncodeValue(&s)
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != byte(TagText) {
		t.Fatal("tag")
	}
}

func TestEncodeValue_Unsupported(t *testing.T) {
	_, err := EncodeValue(complex(1, 2))
	if err == nil {
		t.Fatal("expected error")
	}
	if !errors.Is(err, ErrUnsupportedParam) {
		t.Errorf("expected ErrUnsupportedParam, got %v", err)
	}
}

func TestParamFixtureManifest(t *testing.T) {
	manifest := loadParamFixtures(t)
	for _, fixture := range manifest.Values {
		got, err := EncodeValue(goFixtureValue(t, fixture.Name))
		if err != nil {
			t.Fatalf("%s: encode: %v", fixture.Name, err)
		}
		want, err := hex.DecodeString(fixture.RedwireHex)
		if err != nil {
			t.Fatalf("%s: fixture hex: %v", fixture.Name, err)
		}
		if !bytes.Equal(got, want) {
			t.Fatalf("%s: got %x want %x", fixture.Name, got, want)
		}
	}

	for _, fixture := range manifest.Queries {
		params := make([]any, len(fixture.Params))
		for i, name := range fixture.Params {
			params[i] = goFixtureValue(t, name)
		}
		got, err := EncodeQueryWithParams(fixture.SQL, params)
		if err != nil {
			t.Fatalf("%s: encode query: %v", fixture.Name, err)
		}
		want, err := hex.DecodeString(fixture.RedwireHex)
		if err != nil {
			t.Fatalf("%s: fixture hex: %v", fixture.Name, err)
		}
		if !bytes.Equal(got, want) {
			t.Fatalf("%s: got %x want %x", fixture.Name, got, want)
		}
	}
}

func TestEncodeQueryWithParams_Empty(t *testing.T) {
	got, err := EncodeQueryWithParams("SELECT 1", nil)
	if err != nil {
		t.Fatal(err)
	}
	// Layout: [sql_len=8][SELECT 1][param_count=0]
	want := []byte{
		0x08, 0x00, 0x00, 0x00,
		'S', 'E', 'L', 'E', 'C', 'T', ' ', '1',
		0x00, 0x00, 0x00, 0x00,
	}
	if !bytes.Equal(got, want) {
		t.Errorf("got % x want % x", got, want)
	}
}

type paramFixtureManifest struct {
	Values  []paramValueFixture `json:"values"`
	Queries []paramQueryFixture `json:"queries"`
}

type paramValueFixture struct {
	Name       string `json:"name"`
	RedwireHex string `json:"redwire_hex"`
}

type paramQueryFixture struct {
	Name       string   `json:"name"`
	SQL        string   `json:"sql"`
	Params     []string `json:"params"`
	RedwireHex string   `json:"redwire_hex"`
}

func loadParamFixtures(t *testing.T) paramFixtureManifest {
	t.Helper()
	path := filepath.Join("..", "..", "..", "crates", "reddb-wire", "tests", "fixtures", "params", "manifest.json")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var manifest paramFixtureManifest
	if err := json.Unmarshal(data, &manifest); err != nil {
		t.Fatal(err)
	}
	return manifest
}

func goFixtureValue(t *testing.T, name string) any {
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
		return math.SmallestNonzeroFloat64
	case "text_unicode":
		return "héllo"
	case "text_x":
		return "x"
	case "bytes_empty":
		return []byte{}
	case "bytes_deadbeef":
		return []byte{0xde, 0xad, 0xbe, 0xef}
	case "json_nested":
		return map[string]any{"z": []any{float64(1), map[string]any{"deep": []any{true, false}}}, "a": nil}
	case "timestamp_zero":
		return Timestamp(0)
	case "timestamp_max":
		return Timestamp(math.MaxInt64)
	case "uuid_001122":
		uuid, err := UUIDFromString("00112233-4455-6677-8899-aabbccddeeff")
		if err != nil {
			t.Fatal(err)
		}
		return uuid
	case "vector_empty":
		return []float32{}
	case "vector_three":
		return []float32{1, 2, -0.5}
	default:
		t.Fatalf("unknown fixture %s", name)
		return nil
	}
}

func TestEncodeQueryWithParams_Mixed(t *testing.T) {
	got, err := EncodeQueryWithParams("S", []any{int64(1), "x", nil, true})
	if err != nil {
		t.Fatal(err)
	}
	pos := 0
	if binary.LittleEndian.Uint32(got[pos:]) != 1 {
		t.Errorf("sql_len")
	}
	pos += 4
	if got[pos] != 'S' {
		t.Errorf("sql byte")
	}
	pos++
	if binary.LittleEndian.Uint32(got[pos:]) != 4 {
		t.Errorf("param_count")
	}
	pos += 4
	// param 0: Int(1)
	if got[pos] != byte(TagInt) {
		t.Errorf("param0 tag")
	}
	pos += 9
	// param 1: Text("x")
	if got[pos] != byte(TagText) {
		t.Errorf("param1 tag: %#x at pos %d", got[pos], pos)
	}
	pos += 5 + 1
	// param 2: Null
	if got[pos] != byte(TagNull) {
		t.Errorf("param2 tag")
	}
	pos++
	// param 3: Bool(true)
	if got[pos] != byte(TagBool) {
		t.Errorf("param3 tag")
	}
}

func TestEncodeQueryWithParams_OverParamCount(t *testing.T) {
	big := make([]any, MaxParamCount+1)
	for i := range big {
		big[i] = int64(0)
	}
	if _, err := EncodeQueryWithParams("S", big); err == nil {
		t.Fatal("expected over-cap error")
	}
}

func TestEncodeQueryWithParams_PropagatesValueError(t *testing.T) {
	_, err := EncodeQueryWithParams("S", []any{complex(1, 2)})
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "param[0]") {
		t.Errorf("expected param[0] in error, got %v", err)
	}
}
