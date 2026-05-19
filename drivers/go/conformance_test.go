package reddb

// SDK Helper Spec — conformance harness (Go driver).
//
// Spec: `docs/spec/sdk-helpers.md` (v1.0). Case IDs in §12 are ported as Go
// test function names (with dots → underscores) so cross-driver dashboards
// line up.
//
// This harness needs a real RedDB server: the Go driver does not embed the
// engine. The harness is opt-in, gated on the same env contract as
// `internal/redserver/` so CI policy is uniform across the Go driver:
//
//   - skipped by default,
//   - skipped when RED_SKIP_SMOKE=1,
//   - runs only when RED_SMOKE=1 *and* RED_BIN=/path/to/red are set.
//
// The harness spawns one engine for the whole package, then runs every case
// against a fresh `red://` connection. Cases are written to be independent
// (no shared collections) so failures isolate cleanly.

import (
	"bytes"
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// --- harness wiring ---------------------------------------------------

type confEngine struct {
	uri  string
	cmd  *exec.Cmd
	logs *bytes.Buffer
}

var (
	confOnce sync.Once
	confEng  *confEngine
	confSkip string
)

func startConfEngine(t *testing.T) *confEngine {
	confOnce.Do(func() {
		if os.Getenv("RED_SKIP_SMOKE") == "1" {
			confSkip = "RED_SKIP_SMOKE=1 set"
			return
		}
		if os.Getenv("RED_SMOKE") != "1" {
			confSkip = "set RED_SMOKE=1 to enable the conformance harness; off by default"
			return
		}
		bin := os.Getenv("RED_BIN")
		if bin == "" {
			confSkip = "set RED_BIN=/path/to/red to run the conformance harness"
			return
		}
		if _, err := os.Stat(bin); err != nil {
			confSkip = fmt.Sprintf("RED_BIN %q not found: %v", bin, err)
			return
		}

		tmpdir, err := os.MkdirTemp("", "reddb-go-conformance-*")
		if err != nil {
			confSkip = fmt.Sprintf("tempdir: %v", err)
			return
		}
		dataPath := filepath.Join(tmpdir, "data.db")

		port, err := pickFreePortConf()
		if err != nil {
			confSkip = fmt.Sprintf("free port: %v", err)
			return
		}
		var logs bytes.Buffer
		cmd := exec.Command(bin, "server",
			"--path", dataPath,
			"--bind", fmt.Sprintf("127.0.0.1:%d", port),
		)
		cmd.Stdout = &logs
		cmd.Stderr = &logs
		if err := cmd.Start(); err != nil {
			confSkip = fmt.Sprintf("start engine: %v", err)
			return
		}
		uri := fmt.Sprintf("red://127.0.0.1:%d", port)

		// Wait for the engine to accept connections.
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		c, err := waitForConfConnect(ctx, uri)
		if err != nil {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
			confSkip = fmt.Sprintf("engine never became ready: %v\nlogs:\n%s", err, logs.String())
			return
		}
		_ = c.Close()
		confEng = &confEngine{uri: uri, cmd: cmd, logs: &logs}
	})
	if confSkip != "" {
		t.Skip(confSkip)
	}
	return confEng
}

// dial returns a fresh client against the shared engine.
func (e *confEngine) dial(t *testing.T) (Conn, *Helpers) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c, err := Connect(ctx, e.uri)
	if err != nil {
		t.Fatalf("connect %s: %v\nlogs:\n%s", e.uri, err, e.logs.String())
	}
	t.Cleanup(func() { _ = c.Close() })
	return c, NewHelpers(c)
}

// uniq returns a per-test unique suffix so cases don't collide on the shared
// engine state.
func uniq(t *testing.T) string {
	t.Helper()
	name := strings.ReplaceAll(t.Name(), "/", "_")
	return strings.ToLower(strings.ReplaceAll(name, ".", "_"))
}

func pickFreePortConf() (int, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0, err
	}
	defer ln.Close()
	return ln.Addr().(*net.TCPAddr).Port, nil
}

func waitForConfConnect(ctx context.Context, uri string) (Conn, error) {
	var lastErr error
	for {
		if err := ctx.Err(); err != nil {
			if lastErr != nil {
				return nil, lastErr
			}
			return nil, err
		}
		c, err := Connect(ctx, uri)
		if err == nil {
			if err := c.Ping(ctx); err == nil {
				return c, nil
			} else {
				lastErr = err
				_ = c.Close()
			}
		} else {
			lastErr = err
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// --- case ID: generic.* -----------------------------------------------

// Case ID: generic.query.no_params
func TestConformance_generic_query_no_params(t *testing.T) {
	e := startConfEngine(t)
	c, _ := e.dial(t)
	ctx := context.Background()
	table := "conf_q_" + uniq(t)
	if _, err := c.Exec(ctx, fmt.Sprintf("CREATE TABLE %s (id INTEGER, name TEXT)", table)); err != nil {
		t.Fatalf("create: %v", err)
	}
	if _, err := c.Exec(ctx, fmt.Sprintf("INSERT INTO %s (id, name) VALUES (1, 'a')", table)); err != nil {
		t.Fatalf("insert: %v", err)
	}
	body, err := c.Query(ctx, fmt.Sprintf("SELECT id, name FROM %s", table))
	if err != nil {
		t.Fatalf("select: %v", err)
	}
	if !strings.Contains(string(body), "\"a\"") {
		t.Fatalf("missing row in: %s", body)
	}
}

// Case ID: generic.query_with.params
func TestConformance_generic_query_with_params(t *testing.T) {
	e := startConfEngine(t)
	c, _ := e.dial(t)
	ctx := context.Background()
	table := "conf_p_" + uniq(t)
	if _, err := c.Exec(ctx, fmt.Sprintf("CREATE TABLE %s (id INTEGER, name TEXT)", table)); err != nil {
		t.Fatalf("create: %v", err)
	}
	if _, err := c.Exec(ctx,
		fmt.Sprintf("INSERT INTO %s (id, name) VALUES ($1, $2)", table),
		int64(42), "alice"); err != nil {
		t.Fatalf("insert with params: %v", err)
	}
	body, err := c.Query(ctx,
		fmt.Sprintf("SELECT name FROM %s WHERE id = $1", table),
		int64(42))
	if err != nil {
		t.Fatalf("select with params: %v", err)
	}
	if !strings.Contains(string(body), "alice") {
		t.Fatalf("expected alice row: %s", body)
	}
}

// Case ID: generic.insert.rid
func TestConformance_generic_insert_rid(t *testing.T) {
	e := startConfEngine(t)
	c, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_ins_" + uniq(t)
	// Ensure collection exists by going through documents helper (the generic
	// `Insert(collection, payload)` wire path on Go is row-shaped and does
	// not return a rid; spec §3.3 wraps to `documents.insert` semantically).
	r, err := h.Documents().Insert(ctx, coll, map[string]any{"name": "eve"})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	if r.Affected != 1 {
		t.Fatalf("affected = %d", r.Affected)
	}
	if r.RID == "" {
		t.Fatalf("rid empty")
	}
	_ = c
}

// Case ID: generic.bulk_insert.rids
func TestConformance_generic_bulk_insert_rids(t *testing.T) {
	e := startConfEngine(t)
	c, _ := e.dial(t)
	ctx := context.Background()
	coll := "conf_bulk_" + uniq(t)
	// Touch the collection.
	if err := c.Insert(ctx, coll, map[string]any{"seed": true}); err != nil {
		t.Fatalf("seed: %v", err)
	}

	empty, err := c.BulkInsert(ctx, coll, []any{})
	if err != nil {
		t.Fatalf("empty bulk: %v", err)
	}
	if empty.Affected != 0 || len(empty.IDs) != 0 {
		t.Fatalf("empty bulk: %+v", empty)
	}

	payloads := []any{
		map[string]any{"idx": 0},
		map[string]any{"idx": 1},
		map[string]any{"idx": 2},
	}
	got, err := c.BulkInsert(ctx, coll, payloads)
	if err != nil {
		t.Fatalf("bulk: %v", err)
	}
	if got.Affected != 3 {
		t.Fatalf("affected: %+v", got)
	}
	if len(got.IDs) != 3 {
		t.Fatalf("ids: %+v", got)
	}
	seen := map[string]bool{}
	for _, id := range got.IDs {
		if seen[id] {
			t.Fatalf("duplicate id %q in %+v", id, got.IDs)
		}
		seen[id] = true
	}
}

// Case ID: generic.delete
func TestConformance_generic_delete(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_del_" + uniq(t)
	ins, err := h.Documents().Insert(ctx, coll, map[string]any{"k": "v"})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	r, err := h.Documents().Delete(ctx, coll, ins.RID)
	if err != nil {
		t.Fatalf("delete: %v", err)
	}
	if r.Affected != 1 || !r.Deleted {
		t.Fatalf("envelope: %+v", r)
	}
}

// --- case ID: documents.* ---------------------------------------------

// Case ID: documents.crud_nested_patch
func TestConformance_documents_crud_nested_patch(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_doc_" + uniq(t)
	docs := h.Documents()

	ins, err := docs.Insert(ctx, coll, map[string]any{
		"event_type": "login",
		"attempts":   2,
		"success":    true,
	})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	if ins.RID == "" {
		t.Fatalf("rid empty")
	}

	got, err := docs.Get(ctx, coll, ins.RID)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got["event_type"] != "login" {
		t.Fatalf("event_type lost: %+v", got)
	}

	list, err := docs.List(ctx, coll, ListOptions{})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(list.Items) == 0 {
		t.Fatalf("list empty")
	}

	patched, err := docs.Patch(ctx, coll, ins.RID, map[string]any{"attempts": 3})
	if err != nil {
		t.Fatalf("patch: %v", err)
	}
	// Spec §4.4: top-level merge MUST preserve unrelated fields.
	if patched["event_type"] != "login" {
		t.Fatalf("patch dropped event_type: %+v", patched)
	}

	del, err := docs.Delete(ctx, coll, ins.RID)
	if err != nil {
		t.Fatalf("delete: %v", err)
	}
	if del.Affected != 1 || !del.Deleted {
		t.Fatalf("del envelope: %+v", del)
	}
}

// Case ID: documents.delete_missing_no_error
func TestConformance_documents_delete_missing_no_error(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_doc_miss_" + uniq(t)
	// Touch the collection.
	ins, err := h.Documents().Insert(ctx, coll, map[string]any{"k": "v"})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	if _, err := h.Documents().Delete(ctx, coll, ins.RID); err != nil {
		t.Fatalf("first delete: %v", err)
	}
	r, err := h.Documents().Delete(ctx, coll, "rid_that_does_not_exist")
	if err != nil {
		t.Fatalf("delete missing must not error: %v", err)
	}
	if r.Affected != 0 || r.Deleted {
		t.Fatalf("envelope: %+v", r)
	}
}

// Case ID: documents.patch_empty_rejects
func TestConformance_documents_patch_empty_rejects(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_doc_pe_" + uniq(t)
	ins, err := h.Documents().Insert(ctx, coll, map[string]any{"k": "v"})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	_, err = h.Documents().Patch(ctx, coll, ins.RID, map[string]any{})
	if !IsCode(err, CodeInvalidArgument) {
		t.Fatalf("want INVALID_ARGUMENT, got %v", err)
	}
}

// --- case ID: kv.* ----------------------------------------------------

// Case ID: kv.exact_key_round_trip
func TestConformance_kv_exact_key_round_trip(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_kv_" + uniq(t)
	kv := h.KV()
	const key = "characters:hansel"
	if err := kv.Set(ctx, key, "witch", SetOptions{Collection: coll}); err != nil {
		t.Fatalf("set: %v", err)
	}
	got, err := kv.Get(ctx, key, coll)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got != "witch" {
		t.Fatalf("got %v", got)
	}
}

// Case ID: kv.missing_get_returns_none
func TestConformance_kv_missing_get_returns_none(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_kv_miss_" + uniq(t)
	kv := h.KV()
	if err := kv.Set(ctx, "seed", "v", SetOptions{Collection: coll}); err != nil {
		t.Fatalf("seed: %v", err)
	}
	got, err := kv.Get(ctx, "never:set", coll)
	if err != nil {
		t.Fatalf("missing get must not error: %v", err)
	}
	if got != nil {
		t.Fatalf("expected nil, got %v", got)
	}
}

// Case ID: kv.delete_returns_envelope
func TestConformance_kv_delete_returns_envelope(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_kv_del_" + uniq(t)
	kv := h.KV()
	if err := kv.Set(ctx, "k", "v", SetOptions{Collection: coll}); err != nil {
		t.Fatalf("set: %v", err)
	}
	r, err := kv.Delete(ctx, "k", coll)
	if err != nil {
		t.Fatalf("delete: %v", err)
	}
	if r.Affected != 1 || !r.Deleted {
		t.Fatalf("first delete envelope: %+v", r)
	}
	r2, err := kv.Delete(ctx, "k", coll)
	if err != nil {
		t.Fatalf("second delete must not error: %v", err)
	}
	if r2.Affected != 0 || r2.Deleted {
		t.Fatalf("second delete envelope: %+v", r2)
	}
}

// --- case ID: queues.* ------------------------------------------------

// Case ID: queues.fifo_peek_pop_len
func TestConformance_queues_fifo_peek_pop_len(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	name := "conf_q_fifo_" + uniq(t)
	q := h.Queues()
	if err := q.Create(ctx, name); err != nil {
		t.Fatalf("create: %v", err)
	}
	if _, err := q.Push(ctx, name, map[string]any{"n": 1}); err != nil {
		t.Fatalf("push1: %v", err)
	}
	if _, err := q.Push(ctx, name, map[string]any{"n": 2}); err != nil {
		t.Fatalf("push2: %v", err)
	}
	n, err := q.Len(ctx, name)
	if err != nil || n != 2 {
		t.Fatalf("len: %d %v", n, err)
	}
	peeked, err := q.Peek(ctx, name, 1)
	if err != nil {
		t.Fatalf("peek: %v", err)
	}
	if len(peeked) != 1 {
		t.Fatalf("peek items: %+v", peeked)
	}
	// peek MUST NOT decrement length.
	if n, _ := q.Len(ctx, name); n != 2 {
		t.Fatalf("len after peek: %d", n)
	}
	popped, err := q.Pop(ctx, name, 1)
	if err != nil || len(popped) != 1 {
		t.Fatalf("pop: %v %+v", err, popped)
	}
	if n, _ := q.Len(ctx, name); n != 1 {
		t.Fatalf("len after pop: %d", n)
	}
}

// Case ID: queues.empty_pop_returns_empty
func TestConformance_queues_empty_pop_returns_empty(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	name := "conf_q_empty_" + uniq(t)
	q := h.Queues()
	if err := q.Create(ctx, name); err != nil {
		t.Fatalf("create: %v", err)
	}
	out, err := q.Pop(ctx, name)
	if err != nil {
		t.Fatalf("pop on empty must not error: %v", err)
	}
	if len(out) != 0 {
		t.Fatalf("expected empty, got %+v", out)
	}
}

// Case ID: queues.purge_resets_len
func TestConformance_queues_purge_resets_len(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	name := "conf_q_purge_" + uniq(t)
	q := h.Queues()
	if err := q.Create(ctx, name); err != nil {
		t.Fatalf("create: %v", err)
	}
	for i := 0; i < 3; i++ {
		if _, err := q.Push(ctx, name, map[string]any{"i": i}); err != nil {
			t.Fatalf("push: %v", err)
		}
	}
	if n, _ := q.Len(ctx, name); n != 3 {
		t.Fatalf("len before purge: %d", n)
	}
	if _, err := q.Purge(ctx, name); err != nil {
		t.Fatalf("purge: %v", err)
	}
	if n, _ := q.Len(ctx, name); n != 0 {
		t.Fatalf("len after purge: %d", n)
	}
}

// --- case ID: tx.* ----------------------------------------------------

// Case ID: tx.commit_persists
func TestConformance_tx_commit_persists(t *testing.T) {
	e := startConfEngine(t)
	c, h := e.dial(t)
	ctx := context.Background()
	table := "conf_tx_commit_" + uniq(t)
	if _, err := c.Exec(ctx, fmt.Sprintf("CREATE TABLE %s (name TEXT)", table)); err != nil {
		t.Fatalf("create: %v", err)
	}
	tx := h.Tx()
	if err := tx.Begin(ctx); err != nil {
		t.Fatalf("begin: %v", err)
	}
	if _, err := c.Exec(ctx, fmt.Sprintf("INSERT INTO %s (name) VALUES ('keep')", table)); err != nil {
		t.Fatalf("insert: %v", err)
	}
	if err := tx.Commit(ctx); err != nil {
		t.Fatalf("commit: %v", err)
	}
	body, err := c.Query(ctx, fmt.Sprintf("SELECT name FROM %s WHERE name = 'keep'", table))
	if err != nil {
		t.Fatalf("select: %v", err)
	}
	if !strings.Contains(string(body), "keep") {
		t.Fatalf("commit did not persist: %s", body)
	}
}

// Case ID: tx.rollback_discards
func TestConformance_tx_rollback_discards(t *testing.T) {
	e := startConfEngine(t)
	c, h := e.dial(t)
	ctx := context.Background()
	table := "conf_tx_rb_" + uniq(t)
	if _, err := c.Exec(ctx, fmt.Sprintf("CREATE TABLE %s (name TEXT)", table)); err != nil {
		t.Fatalf("create: %v", err)
	}
	tx := h.Tx()
	if err := tx.Begin(ctx); err != nil {
		t.Fatalf("begin: %v", err)
	}
	if _, err := c.Exec(ctx, fmt.Sprintf("INSERT INTO %s (name) VALUES ('drop')", table)); err != nil {
		t.Fatalf("insert: %v", err)
	}
	if err := tx.Rollback(ctx); err != nil {
		t.Fatalf("rollback: %v", err)
	}
	body, err := c.Query(ctx, fmt.Sprintf("SELECT name FROM %s WHERE name = 'drop'", table))
	if err != nil {
		t.Fatalf("select: %v", err)
	}
	if strings.Contains(string(body), "drop") {
		t.Fatalf("rollback did not discard: %s", body)
	}
}

// --- case ID: errors.* ------------------------------------------------

// Case ID: errors.not_found.document_get
func TestConformance_errors_not_found_document_get(t *testing.T) {
	e := startConfEngine(t)
	_, h := e.dial(t)
	ctx := context.Background()
	coll := "conf_err_nf_" + uniq(t)
	ins, err := h.Documents().Insert(ctx, coll, map[string]any{"k": "v"})
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	if _, err := h.Documents().Delete(ctx, coll, ins.RID); err != nil {
		t.Fatalf("delete: %v", err)
	}
	_, err = h.Documents().Get(ctx, coll, "rid_definitely_missing")
	if !IsCode(err, CodeNotFound) {
		t.Fatalf("want NOT_FOUND, got %v", err)
	}
}

// --- case ID: wire.* (provisional namespaces — SQL only in v1.0) -----

// Case ID: wire.probabilistic.hll_round_trip
func TestConformance_wire_probabilistic_hll_round_trip(t *testing.T) {
	e := startConfEngine(t)
	c, _ := e.dial(t)
	ctx := context.Background()
	name := "conf_hll_" + uniq(t)
	if _, err := c.Exec(ctx, "CREATE HLL "+name); err != nil {
		t.Fatalf("create hll: %v", err)
	}
	if _, err := c.Exec(ctx, fmt.Sprintf("HLL ADD %s 'alice' 'bob' 'alice'", name)); err != nil {
		t.Fatalf("hll add: %v", err)
	}
	body, err := c.Query(ctx, "HLL COUNT "+name)
	if err != nil {
		t.Fatalf("hll count: %v", err)
	}
	// Spec accepts either `count` or `cardinality` as the projected column.
	s := string(body)
	if !strings.Contains(s, "count") && !strings.Contains(s, "cardinality") {
		t.Fatalf("expected count/cardinality column in: %s", s)
	}
}
