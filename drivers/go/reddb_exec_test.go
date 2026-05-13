package reddb

import "testing"

func TestExecResultFromJSONReadsAffectedRows(t *testing.T) {
	result, err := execResultFromJSON([]byte(`{"affected_rows":2}`))
	if err != nil {
		t.Fatal(err)
	}
	if result.RowsAffected() != 2 {
		t.Fatalf("affected = %d", result.RowsAffected())
	}
	if string(result.Raw) != `{"affected_rows":2}` {
		t.Fatalf("raw = %q", result.Raw)
	}
}

func TestExecResultFromJSONReadsAffectedAliasAndEnvelope(t *testing.T) {
	result, err := execResultFromJSON([]byte(`{"ok":true,"result":{"affected":3}}`))
	if err != nil {
		t.Fatal(err)
	}
	if result.RowsAffected() != 3 {
		t.Fatalf("affected = %d", result.RowsAffected())
	}
}
