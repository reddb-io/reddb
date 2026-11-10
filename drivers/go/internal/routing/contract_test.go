package routing

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"reflect"
	"testing"
)

func TestCanonicalTopologyFixtures(t *testing.T) {
	data, err := os.ReadFile("../../../../testdata/conformance/topology/manifest.json")
	if err != nil {
		t.Fatal(err)
	}
	var manifest struct {
		MetadataKey string `json:"metadata_key"`
		Cases       []struct {
			Name     string
			Hex      string
			Error    bool
			Expected any
		}
	}
	if err = json.Unmarshal(data, &manifest); err != nil {
		t.Fatal(err)
	}
	if manifest.MetadataKey != MetadataKey {
		t.Fatal("metadata selector drift")
	}
	for _, fixture := range manifest.Cases {
		t.Run(fixture.Name, func(t *testing.T) {
			bytes, err := hex.DecodeString(fixture.Hex)
			if err != nil {
				t.Fatal(err)
			}
			topology, err := Decode(bytes)
			if fixture.Error {
				if err == nil {
					t.Fatal("accepted malformed topology")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			encoded, err := json.Marshal(topology)
			if err != nil {
				t.Fatal(err)
			}
			var actual any
			if err = json.Unmarshal(encoded, &actual); err != nil {
				t.Fatal(err)
			}
			if !reflect.DeepEqual(actual, fixture.Expected) {
				t.Fatalf("decoded mismatch: %s", encoded)
			}
		})
	}
}

func TestSharedSQLPolicy(t *testing.T) {
	data, err := os.ReadFile("../../../../testdata/conformance/topology/sql-policy.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		SQL         string `json:"sql"`
		ReplicaRead bool   `json:"replica_read"`
	}
	if err = json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	for _, fixture := range cases {
		_, read := SQLPolicy(fixture.SQL)
		if read != fixture.ReplicaRead {
			t.Errorf("%s: got %v", fixture.SQL, read)
		}
	}
}
