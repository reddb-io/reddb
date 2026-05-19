package reddb

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

// fakeQuerier records each SQL call and replays scripted JSON responses.
type fakeQuerier struct {
	calls   []fakeCall
	replies [][]byte
	errs    []error
}

type fakeCall struct {
	sql    string
	params []any
}

func (f *fakeQuerier) Query(_ context.Context, sql string, params ...any) ([]byte, error) {
	f.calls = append(f.calls, fakeCall{sql: sql, params: params})
	var body []byte
	if len(f.replies) > 0 {
		body, f.replies = f.replies[0], f.replies[1:]
	}
	var err error
	if len(f.errs) > 0 {
		err, f.errs = f.errs[0], f.errs[1:]
	}
	return body, err
}

func reply(t *testing.T, v any) []byte {
	t.Helper()
	bs, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshal reply: %v", err)
	}
	return bs
}

// ----------------------------------------------------------------- KV path

func TestKVPath_QuotesNamespacedKeys(t *testing.T) {
	p, err := KVPath("kv_default", "corpus:version")
	if err != nil {
		t.Fatalf("unexpected: %v", err)
	}
	if p != "kv_default.'corpus:version'" {
		t.Fatalf("got %q", p)
	}
}

func TestKVPath_PreservesDotsAndSlashes(t *testing.T) {
	p, _ := KVPath("kv_default", "a/b.c")
	if p != "kv_default.'a/b.c'" {
		t.Fatalf("got %q", p)
	}
}

func TestKVPath_RejectsBadCollection(t *testing.T) {
	_, err := KVPath("bad-name!", "k")
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT, got %v", err)
	}
}

func TestKVValueLiteral(t *testing.T) {
	cases := []struct {
		name string
		in   any
		want string
	}{
		{"nil", nil, "NULL"},
		{"true", true, "true"},
		{"false", false, "false"},
		{"int", int64(42), "42"},
		{"text", "hi", "'hi'"},
		{"escape", "o'reilly", "'o''reilly'"},
		{"object", map[string]any{"a": 1}, `'{"a":1}'`},
	}
	for _, c := range cases {
		got, err := kvValueLiteral(c.in)
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		if got != c.want {
			t.Fatalf("%s: got %q want %q", c.name, got, c.want)
		}
	}
}

// ----------------------------------------------------------------- KV ops

func TestKV_Set_EmitsExactKeyPath(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{})}}
	h := NewHelpers(fq).KV()
	if err := h.Set(context.Background(), "characters:hansel", "ok"); err != nil {
		t.Fatalf("set: %v", err)
	}
	sql := fq.calls[0].sql
	if !strings.Contains(sql, "kv_default.'characters:hansel'") {
		t.Fatalf("missing exact key: %s", sql)
	}
	if !strings.Contains(sql, "= 'ok'") {
		t.Fatalf("missing value: %s", sql)
	}
}

func TestKV_Get_ReturnsValueOrNil(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{"rows": []any{map[string]any{"value": "v"}}}),
		reply(t, map[string]any{"rows": []any{}}),
	}}
	h := NewHelpers(fq).KV()
	got, err := h.Get(context.Background(), "k")
	if err != nil || got != "v" {
		t.Fatalf("got %v %v", got, err)
	}
	got, err = h.Get(context.Background(), "k2")
	if err != nil || got != nil {
		t.Fatalf("got %v %v", got, err)
	}
}

func TestKV_Exists_UsesGet(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{"rows": []any{map[string]any{"value": "v"}}}),
		reply(t, map[string]any{"rows": []any{}}),
	}}
	h := NewHelpers(fq).KV()
	r, _ := h.Exists(context.Background(), "k")
	if !r.Exists {
		t.Fatal("want exists=true")
	}
	r, _ = h.Exists(context.Background(), "k2")
	if r.Exists {
		t.Fatal("want exists=false")
	}
}

func TestKV_List_FiltersByPrefixWithoutRewriting(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{"rows": []any{
		map[string]any{"key": "a:1", "value": 1},
		map[string]any{"key": "b:1", "value": 2},
		map[string]any{"key": "a:2", "value": 3},
	}})}}
	h := NewHelpers(fq).KV()
	out, err := h.List(context.Background(), KVListOptions{Prefix: "a:"})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(out.Items) != 2 {
		t.Fatalf("want 2 rows, got %d", len(out.Items))
	}
	if out.Items[0]["key"] != "a:1" || out.Items[1]["key"] != "a:2" {
		t.Fatalf("rows: %+v", out.Items)
	}
}

func TestKV_List_RejectsNegativeLimit(t *testing.T) {
	h := NewHelpers(&fakeQuerier{}).KV()
	_, err := h.List(context.Background(), KVListOptions{Limit: -1})
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT, got %v", err)
	}
}

// ----------------------------------------------------------------- Queue

func TestQueue_Push_EmitsPriorityAndPayload(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{"affected": 1})}}
	q := NewHelpers(fq).Queue()
	p := 5
	if _, err := q.Push(context.Background(), "jobs", map[string]any{"id": 1}, PushOptions{Priority: &p}); err != nil {
		t.Fatalf("push: %v", err)
	}
	sql := fq.calls[0].sql
	if !strings.HasPrefix(sql, "QUEUE PUSH jobs ") {
		t.Fatalf("prefix: %s", sql)
	}
	if !strings.Contains(sql, "PRIORITY 5") {
		t.Fatalf("priority: %s", sql)
	}
	if !strings.Contains(sql, `{"id":1}`) {
		t.Fatalf("payload: %s", sql)
	}
}

func TestQueue_Len_ReturnsInt(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{"rows": []any{map[string]any{"len": 3}}})}}
	q := NewHelpers(fq).Queue()
	n, err := q.Len(context.Background(), "jobs")
	if err != nil || n != 3 {
		t.Fatalf("len: %d %v", n, err)
	}
}

func TestQueue_Pop_ReturnsPayloads(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{"rows": []any{
		map[string]any{"payload": "a"},
		map[string]any{"payload": "b"},
	}})}}
	q := NewHelpers(fq).Queue()
	out, err := q.Pop(context.Background(), "jobs", 2)
	if err != nil {
		t.Fatalf("pop: %v", err)
	}
	if len(out) != 2 || out[0] != "a" || out[1] != "b" {
		t.Fatalf("payloads: %v", out)
	}
}

func TestQueue_Pop_RejectsNegativeCount(t *testing.T) {
	q := NewHelpers(&fakeQuerier{}).Queue()
	_, err := q.Pop(context.Background(), "jobs", -1)
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT: %v", err)
	}
}

func TestQueue_Push_RejectsInvalidIdentifier(t *testing.T) {
	q := NewHelpers(&fakeQuerier{}).Queue()
	_, err := q.Push(context.Background(), "bad-name!", "x")
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT: %v", err)
	}
}

// ----------------------------------------------------------------- Documents

func TestDocuments_Insert_ReturnsRIDEnvelope(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{"rows": []any{}, "affected": 0}),
		reply(t, map[string]any{
			"rows":     []any{map[string]any{"rid": "doc-1", "body": map[string]any{"name": "alice"}}},
			"affected": 1,
		}),
	}}
	d := NewHelpers(fq).Documents()
	out, err := d.Insert(context.Background(), "people", map[string]any{"name": "alice"})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	if out.Affected != 1 || out.RID != "doc-1" {
		t.Fatalf("envelope: %+v", out)
	}
	if out.Item["rid"] != "doc-1" {
		t.Fatalf("item: %+v", out.Item)
	}
}

func TestDocuments_Get_RaisesNotFoundOnMissing(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{"rows": []any{}})}}
	d := NewHelpers(fq).Documents()
	_, err := d.Get(context.Background(), "people", "doc-1")
	if !IsCode(err, CodeNotFound) {
		t.Fatalf("want NOT_FOUND: %v", err)
	}
}

func TestDocuments_Patch_RejectsJSONPointerPaths(t *testing.T) {
	d := NewHelpers(&fakeQuerier{}).Documents()
	_, err := d.Patch(context.Background(), "people", "doc-1", map[string]any{"a/b": 1})
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT: %v", err)
	}
}

func TestDocuments_List_OrdersByRIDByDefault(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{"rows": []any{
		map[string]any{"rid": "a"},
		map[string]any{"rid": "b"},
	}})}}
	d := NewHelpers(fq).Documents()
	out, err := d.List(context.Background(), "people", ListOptions{})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(out.Items) != 2 {
		t.Fatalf("rows: %+v", out.Items)
	}
	sql := fq.calls[0].sql
	if !strings.Contains(sql, "ORDER BY rid ASC") {
		t.Fatalf("ordering: %s", sql)
	}
}

func TestDocuments_Insert_PassesThroughExistingCollection(t *testing.T) {
	fq := &fakeQuerier{
		replies: [][]byte{
			reply(t, map[string]any{}),
			reply(t, map[string]any{"rows": []any{map[string]any{"rid": "x"}}, "affected": 1}),
		},
		errs: []error{errors.New("collection already exists"), nil},
	}
	d := NewHelpers(fq).Documents()
	if _, err := d.Insert(context.Background(), "people", map[string]any{"a": 1}); err != nil {
		t.Fatalf("insert: %v", err)
	}
}

// ----------------------------------------------------------------- Tx

func TestTx_Begin_Commit_Rollback_EmitsSQL(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{}),
		reply(t, map[string]any{}),
		reply(t, map[string]any{}),
	}}
	tx := NewHelpers(fq).Tx()
	ctx := context.Background()
	if err := tx.Begin(ctx); err != nil {
		t.Fatalf("begin: %v", err)
	}
	if err := tx.Commit(ctx); err != nil {
		t.Fatalf("commit: %v", err)
	}
	if err := tx.Rollback(ctx); err != nil {
		t.Fatalf("rollback: %v", err)
	}
	want := []string{"BEGIN", "COMMIT", "ROLLBACK"}
	for i, c := range fq.calls {
		if c.sql != want[i] {
			t.Fatalf("call %d: got %q want %q", i, c.sql, want[i])
		}
	}
}

func TestTx_Run_CommitsOnSuccess(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{}), // BEGIN
		reply(t, map[string]any{}), // body work
		reply(t, map[string]any{}), // COMMIT
	}}
	tx := NewHelpers(fq).Tx()
	err := tx.Run(context.Background(), func(child *TxClient) error {
		_, err := child.q.Query(context.Background(), "INSERT INTO t VALUES (1)")
		return err
	})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if fq.calls[0].sql != "BEGIN" || fq.calls[2].sql != "COMMIT" {
		t.Fatalf("expected BEGIN..COMMIT, got %+v", fq.calls)
	}
}

func TestTx_Run_RollsBackOnError(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{}), // BEGIN
		reply(t, map[string]any{}), // ROLLBACK
	}}
	tx := NewHelpers(fq).Tx()
	err := tx.Run(context.Background(), func(_ *TxClient) error {
		return errors.New("boom")
	})
	if err == nil || err.Error() != "boom" {
		t.Fatalf("expected boom, got %v", err)
	}
	if fq.calls[len(fq.calls)-1].sql != "ROLLBACK" {
		t.Fatalf("expected ROLLBACK last, got %+v", fq.calls)
	}
}

// ----------------------------------------------------------------- Documents extra

func TestDocuments_Patch_EmptyRejects(t *testing.T) {
	d := NewHelpers(&fakeQuerier{}).Documents()
	_, err := d.Patch(context.Background(), "people", "doc-1", map[string]any{})
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT, got %v", err)
	}
}

func TestDocuments_Delete_PopulatesDeletedFlag(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{
		reply(t, map[string]any{"affected": 1}),
		reply(t, map[string]any{"affected": 0}),
	}}
	d := NewHelpers(fq).Documents()
	r, _ := d.Delete(context.Background(), "people", "rid-1")
	if !r.Deleted || r.Affected != 1 {
		t.Fatalf("hit: %+v", r)
	}
	r, _ = d.Delete(context.Background(), "people", "rid-missing")
	if r.Deleted || r.Affected != 0 {
		t.Fatalf("miss: %+v", r)
	}
}

// ----------------------------------------------------------------- Queues extra

func TestQueue_Create_EmitsIfNotExists(t *testing.T) {
	fq := &fakeQuerier{replies: [][]byte{reply(t, map[string]any{})}}
	q := NewHelpers(fq).Queues()
	if err := q.Create(context.Background(), "jobs"); err != nil {
		t.Fatalf("create: %v", err)
	}
	if fq.calls[0].sql != "CREATE QUEUE IF NOT EXISTS jobs" {
		t.Fatalf("sql: %q", fq.calls[0].sql)
	}
}

func TestQueue_Create_RejectsBadIdentifier(t *testing.T) {
	q := NewHelpers(&fakeQuerier{}).Queues()
	if err := q.Create(context.Background(), "bad-name!"); !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT, got %v", err)
	}
}

// ---------------------------------------------------------- spec constant

func TestHelperSpecVersion(t *testing.T) {
	if HelperSpecVersion != "1.0" {
		t.Fatalf("HelperSpecVersion: got %q want %q", HelperSpecVersion, "1.0")
	}
}

// ---------------------------------------------------------- decode helpers

func TestAffectedFromBody_HandlesNestedResult(t *testing.T) {
	body := reply(t, map[string]any{"result": map[string]any{"affected": 7}})
	if affectedFromBody(body) != 7 {
		t.Fatalf("nested affected")
	}
}
