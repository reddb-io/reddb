package reddb

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
)

// SDK Helper Spec v0.1 — rich helper surface on top of the transport-agnostic
// Conn. Helpers compile SQL strings against the engine; the same wire request
// works across RedWire and HTTP. See docs/clients/sdk-helper-spec.md.

// Querier is the minimal contract helpers need. Conn satisfies it; tests pass
// fakes that record SQL.
type Querier interface {
	Query(ctx context.Context, sql string, params ...any) ([]byte, error)
}

// Helpers groups the rich namespaces (`Documents`, `KV`, `Queue`) bound to a
// single transport. Helpers are stateless — safe to construct per call.
type Helpers struct{ q Querier }

// NewHelpers wraps any Querier (typically a Conn) with the rich helper surface.
func NewHelpers(q Querier) *Helpers { return &Helpers{q: q} }

// Documents returns the document namespace client.
func (h *Helpers) Documents() *DocumentClient { return &DocumentClient{q: h.q} }

// KV returns the KV namespace client bound to the default collection
// (``kv_default``).
func (h *Helpers) KV() *KVClient { return &KVClient{q: h.q, Collection: "kv_default"} }

// Queue returns the queue namespace client.
func (h *Helpers) Queue() *QueueClient { return &QueueClient{q: h.q} }

// --- Envelopes -------------------------------------------------------

// InsertResult is the spec envelope for single-item inserts.
type InsertResult struct {
	Affected uint64         `json:"affected"`
	RID      string         `json:"rid"`
	Item     map[string]any `json:"item,omitempty"`
}

// DeleteResult is the spec envelope for delete helpers.
type DeleteResult struct {
	Affected uint64 `json:"affected"`
}

// ExistsResult is the spec envelope for existence checks.
type ExistsResult struct {
	Exists bool `json:"exists"`
}

// ListResult is the spec envelope for list helpers.
type ListResult struct {
	Items      []map[string]any `json:"items"`
	NextCursor string           `json:"next_cursor,omitempty"`
}

// QueuePushResult is the spec envelope for queue push helpers.
type QueuePushResult struct {
	Affected uint64 `json:"affected"`
	RID      string `json:"rid,omitempty"`
}

// --- Documents -------------------------------------------------------

// DocumentClient implements `documents.*` from the SDK Helper Spec.
type DocumentClient struct{ q Querier }

// Insert creates one document item and returns the spec InsertResult.
func (d *DocumentClient) Insert(ctx context.Context, collection string, document map[string]any) (*InsertResult, error) {
	if document == nil {
		return nil, NewError(CodeInvalidArgument, "documents.insert document must be an object")
	}
	if err := d.ensureCollection(ctx, collection); err != nil {
		return nil, err
	}
	jsonLit, err := jsonLiteral(document)
	if err != nil {
		return nil, err
	}
	sql := fmt.Sprintf("INSERT INTO %s DOCUMENT (body) VALUES (%s) RETURNING *",
		sqlIdentifierPath(collection), jsonLit)
	body, err := d.q.Query(ctx, sql)
	if err != nil {
		return nil, err
	}
	row, affected := firstRow(body)
	if row == nil || row["rid"] == nil {
		return nil, NewError(CodeInvalidResponse, "documents.insert expected one returned item with rid")
	}
	rid, _ := ridString(row["rid"])
	if affected == 0 {
		affected = 1
	}
	return &InsertResult{Affected: affected, RID: rid, Item: row}, nil
}

// Get fetches one document by rid. Returns CodeNotFound when missing.
func (d *DocumentClient) Get(ctx context.Context, collection, rid string) (map[string]any, error) {
	sql := fmt.Sprintf("SELECT * FROM %s WHERE rid = $1 LIMIT 1", sqlIdentifierPath(collection))
	body, err := d.q.Query(ctx, sql, rid)
	if err != nil {
		return nil, err
	}
	row, _ := firstRow(body)
	if row == nil {
		return nil, NewError(CodeNotFound, fmt.Sprintf("document %q was not found", rid))
	}
	return row, nil
}

// ListOptions tweaks list result ordering and bounds.
type ListOptions struct {
	Limit   int
	OrderBy string
	Filter  string
}

// List returns up to Limit rows ordered by OrderBy (default "rid ASC").
func (d *DocumentClient) List(ctx context.Context, collection string, opts ListOptions) (*ListResult, error) {
	limit, err := normalizeLimit(opts.Limit)
	if err != nil {
		return nil, err
	}
	order := opts.OrderBy
	if order == "" {
		order = "rid ASC"
	}
	where := ""
	if opts.Filter != "" {
		where = " WHERE " + opts.Filter
	}
	sql := fmt.Sprintf("SELECT * FROM %s%s ORDER BY %s LIMIT %d",
		sqlIdentifierPath(collection), where, order, limit)
	body, err := d.q.Query(ctx, sql)
	if err != nil {
		return nil, err
	}
	rows := allRows(body)
	return &ListResult{Items: rows}, nil
}

// Patch applies a top-level patch to one document.
func (d *DocumentClient) Patch(ctx context.Context, collection, rid string, patch map[string]any) (map[string]any, error) {
	if patch == nil {
		return nil, NewError(CodeInvalidArgument, "documents.patch patch must be an object")
	}
	if len(patch) == 0 {
		return d.Get(ctx, collection, rid)
	}
	parts := make([]string, 0, len(patch))
	for field, value := range patch {
		if strings.Contains(field, "/") {
			return nil, NewError(CodeInvalidArgument,
				"documents.patch currently accepts top-level document fields")
		}
		lit, err := valueLiteral(value)
		if err != nil {
			return nil, err
		}
		parts = append(parts, fmt.Sprintf("%s = %s", sqlIdentifier(field), lit))
	}
	sql := fmt.Sprintf("UPDATE %s DOCUMENTS SET %s WHERE rid = $1 RETURNING *",
		sqlIdentifierPath(collection), strings.Join(parts, ", "))
	body, err := d.q.Query(ctx, sql, rid)
	if err != nil {
		return nil, err
	}
	row, _ := firstRow(body)
	if row == nil {
		return nil, NewError(CodeNotFound, fmt.Sprintf("document %q was not found", rid))
	}
	return row, nil
}

// Delete removes a document by rid.
func (d *DocumentClient) Delete(ctx context.Context, collection, rid string) (*DeleteResult, error) {
	sql := fmt.Sprintf("DELETE FROM %s WHERE rid = $1", sqlIdentifierPath(collection))
	body, err := d.q.Query(ctx, sql, rid)
	if err != nil {
		return nil, err
	}
	return &DeleteResult{Affected: affectedFromBody(body)}, nil
}

func (d *DocumentClient) ensureCollection(ctx context.Context, collection string) error {
	sql := fmt.Sprintf("CREATE DOCUMENT %s", sqlIdentifierPath(collection))
	_, err := d.q.Query(ctx, sql)
	if err == nil {
		return nil
	}
	if strings.Contains(err.Error(), "already exists") {
		return nil
	}
	return err
}

// --- KV --------------------------------------------------------------

// KVClient implements `kv.*` from the SDK Helper Spec.
type KVClient struct {
	q          Querier
	Collection string
}

// SetOptions controls KV Set/Put behaviour.
type SetOptions struct {
	Collection string
	Tags       []string
	ExpireMs   int64
}

// Set stores an exact key/value pair (alias for Put).
func (k *KVClient) Set(ctx context.Context, key string, value any, opts ...SetOptions) error {
	return k.Put(ctx, key, value, opts...)
}

// Put stores an exact key/value pair, optionally with tags and TTL.
func (k *KVClient) Put(ctx context.Context, key string, value any, opts ...SetOptions) error {
	opt := SetOptions{}
	if len(opts) > 0 {
		opt = opts[0]
	}
	collection := opt.Collection
	if collection == "" {
		collection = k.Collection
	}
	lit, err := kvValueLiteral(value)
	if err != nil {
		return err
	}
	expire := ""
	if opt.ExpireMs > 0 {
		expire = fmt.Sprintf(" EXPIRE %d ms", opt.ExpireMs)
	}
	tagClause := ""
	if len(opt.Tags) > 0 {
		parts := make([]string, len(opt.Tags))
		for i, t := range opt.Tags {
			parts[i] = kvTagLiteral(t)
		}
		tagClause = " TAGS [" + strings.Join(parts, ", ") + "]"
	}
	path, err := KVPath(collection, key)
	if err != nil {
		return err
	}
	sql := fmt.Sprintf("KV PUT %s = %s%s%s", path, lit, expire, tagClause)
	_, err = k.q.Query(ctx, sql)
	return err
}

// Get returns the stored value or `nil` when missing.
func (k *KVClient) Get(ctx context.Context, key string, collection ...string) (any, error) {
	coll := k.Collection
	if len(collection) > 0 && collection[0] != "" {
		coll = collection[0]
	}
	path, err := KVPath(coll, key)
	if err != nil {
		return nil, err
	}
	body, err := k.q.Query(ctx, "KV GET "+path)
	if err != nil {
		return nil, err
	}
	row, _ := firstRow(body)
	if row == nil {
		return nil, nil
	}
	return row["value"], nil
}

// Exists reports whether a key is present.
func (k *KVClient) Exists(ctx context.Context, key string, collection ...string) (*ExistsResult, error) {
	val, err := k.Get(ctx, key, collection...)
	if err != nil {
		return nil, err
	}
	return &ExistsResult{Exists: val != nil}, nil
}

// Delete removes one exact key.
func (k *KVClient) Delete(ctx context.Context, key string, collection ...string) (*DeleteResult, error) {
	coll := k.Collection
	if len(collection) > 0 && collection[0] != "" {
		coll = collection[0]
	}
	path, err := KVPath(coll, key)
	if err != nil {
		return nil, err
	}
	body, err := k.q.Query(ctx, "KV DELETE "+path)
	if err != nil {
		return nil, err
	}
	return &DeleteResult{Affected: affectedFromBody(body)}, nil
}

// KVListOptions controls KV List output.
type KVListOptions struct {
	Collection string
	Limit      int
	Prefix     string
}

// List returns up to Limit rows (default 100), optionally filtered by prefix
// after the server replies (keys are never rewritten by the helper).
func (k *KVClient) List(ctx context.Context, opts KVListOptions) (*ListResult, error) {
	coll := opts.Collection
	if coll == "" {
		coll = k.Collection
	}
	limit, err := normalizeLimit(opts.Limit)
	if err != nil {
		return nil, err
	}
	sql := fmt.Sprintf("SELECT key, value FROM %s ORDER BY key ASC LIMIT %d",
		sqlIdentifier(coll), limit)
	body, err := k.q.Query(ctx, sql)
	if err != nil {
		return nil, err
	}
	rows := allRows(body)
	if opts.Prefix != "" {
		filtered := rows[:0]
		for _, r := range rows {
			key, _ := r["key"].(string)
			if strings.HasPrefix(key, opts.Prefix) {
				filtered = append(filtered, r)
			}
		}
		rows = filtered
	}
	return &ListResult{Items: rows}, nil
}

// --- Queue -----------------------------------------------------------

// QueueClient implements `queue.*` from the SDK Helper Spec.
type QueueClient struct{ q Querier }

// PushOptions controls queue push behaviour.
type PushOptions struct {
	Priority *int
}

// Push enqueues one payload.
func (qc *QueueClient) Push(ctx context.Context, queue string, value any, opts ...PushOptions) (*QueuePushResult, error) {
	if err := assertIdentifier(queue, "queue name"); err != nil {
		return nil, err
	}
	opt := PushOptions{}
	if len(opts) > 0 {
		opt = opts[0]
	}
	lit, err := queueValueLiteral(value)
	if err != nil {
		return nil, err
	}
	priority := ""
	if opt.Priority != nil {
		priority = fmt.Sprintf(" PRIORITY %d", *opt.Priority)
	}
	sql := fmt.Sprintf("QUEUE PUSH %s %s%s", sqlIdentifier(queue), lit, priority)
	body, err := qc.q.Query(ctx, sql)
	if err != nil {
		return nil, err
	}
	res := &QueuePushResult{Affected: affectedFromBody(body)}
	if res.Affected == 0 {
		res.Affected = 1
	}
	if row, _ := firstRow(body); row != nil {
		if rid, ok := ridString(row["rid"]); ok {
			res.RID = rid
		}
	}
	return res, nil
}

// Pop removes and returns the next `count` payloads (default 1).
func (qc *QueueClient) Pop(ctx context.Context, queue string, count ...int) ([]any, error) {
	return qc.fetch(ctx, "POP", queue, count)
}

// Peek returns the next `count` payloads without removing them.
func (qc *QueueClient) Peek(ctx context.Context, queue string, count ...int) ([]any, error) {
	return qc.fetch(ctx, "PEEK", queue, count)
}

func (qc *QueueClient) fetch(ctx context.Context, verb, queue string, count []int) ([]any, error) {
	if err := assertIdentifier(queue, "queue name"); err != nil {
		return nil, err
	}
	suffix := ""
	if len(count) > 0 {
		if count[0] < 0 {
			return nil, NewError(CodeInvalidArgument,
				"queue count must be a non-negative integer")
		}
		suffix = fmt.Sprintf(" COUNT %d", count[0])
	}
	body, err := qc.q.Query(ctx, fmt.Sprintf("QUEUE %s %s%s", verb, sqlIdentifier(queue), suffix))
	if err != nil {
		return nil, err
	}
	rows := allRows(body)
	out := make([]any, 0, len(rows))
	for _, row := range rows {
		out = append(out, row["payload"])
	}
	return out, nil
}

// Len returns the queue length.
func (qc *QueueClient) Len(ctx context.Context, queue string) (uint64, error) {
	if err := assertIdentifier(queue, "queue name"); err != nil {
		return 0, err
	}
	body, err := qc.q.Query(ctx, "QUEUE LEN "+sqlIdentifier(queue))
	if err != nil {
		return 0, err
	}
	row, _ := firstRow(body)
	if row == nil {
		return 0, nil
	}
	switch v := row["len"].(type) {
	case float64:
		return uint64(v), nil
	case json.Number:
		n, _ := v.Int64()
		return uint64(n), nil
	}
	return 0, nil
}

// Purge removes every item in a queue.
func (qc *QueueClient) Purge(ctx context.Context, queue string) (*DeleteResult, error) {
	if err := assertIdentifier(queue, "queue name"); err != nil {
		return nil, err
	}
	body, err := qc.q.Query(ctx, "QUEUE PURGE "+sqlIdentifier(queue))
	if err != nil {
		return nil, err
	}
	return &DeleteResult{Affected: affectedFromBody(body)}, nil
}

// --- pure SQL helpers (unit-testable) --------------------------------

// KVPath builds a fully qualified ``collection.key`` reference, quoting the
// key segment when it contains anything but `[A-Za-z0-9_]`.
func KVPath(collection, key string) (string, error) {
	ident, err := kvIdentifier(collection)
	if err != nil {
		return "", err
	}
	return ident + "." + kvKeySegment(key), nil
}

func kvIdentifier(value string) (string, error) {
	for _, ch := range value {
		if !isIdentChar(ch) {
			return "", NewError(CodeInvalidArgument,
				fmt.Sprintf("invalid KV collection %q: character %q is not supported",
					value, ch))
		}
	}
	return value, nil
}

func kvKeySegment(value string) string {
	if value != "" && allIdentChars(value) {
		return value
	}
	return "'" + strings.ReplaceAll(value, "'", "''") + "'"
}

func kvValueLiteral(value any) (string, error) {
	switch v := value.(type) {
	case nil:
		return "NULL", nil
	case bool:
		if v {
			return "true", nil
		}
		return "false", nil
	case string:
		return "'" + strings.ReplaceAll(v, "'", "''") + "'", nil
	case int, int8, int16, int32, int64:
		return fmt.Sprintf("%d", v), nil
	case uint, uint8, uint16, uint32, uint64:
		return fmt.Sprintf("%d", v), nil
	case float32, float64:
		return fmt.Sprintf("%v", v), nil
	}
	bs, err := json.Marshal(value)
	if err != nil {
		return "", NewError(CodeInvalidArgument, err.Error())
	}
	return "'" + strings.ReplaceAll(string(bs), "'", "''") + "'", nil
}

func kvTagLiteral(tag string) string {
	return "'" + strings.ReplaceAll(tag, "'", "''") + "'"
}

func queueValueLiteral(value any) (string, error) {
	switch v := value.(type) {
	case nil:
		return "NULL", nil
	case bool:
		if v {
			return "true", nil
		}
		return "false", nil
	case string:
		return "'" + strings.ReplaceAll(v, "'", "''") + "'", nil
	case int, int8, int16, int32, int64:
		return fmt.Sprintf("%d", v), nil
	case uint, uint8, uint16, uint32, uint64:
		return fmt.Sprintf("%d", v), nil
	case float32, float64:
		return fmt.Sprintf("%v", v), nil
	}
	bs, err := json.Marshal(value)
	if err != nil {
		return "", NewError(CodeInvalidArgument, err.Error())
	}
	return string(bs), nil
}

func valueLiteral(value any) (string, error) {
	// SQL value literal for arbitrary patch payloads — JSON-encoded objects
	// land as single-quoted JSON, primitives stay literal.
	return kvValueLiteral(value)
}

func jsonLiteral(value any) (string, error) {
	bs, err := json.Marshal(value)
	if err != nil {
		return "", NewError(CodeInvalidArgument, err.Error())
	}
	return "'" + strings.ReplaceAll(string(bs), "'", "''") + "'", nil
}

func sqlIdentifier(value string) string {
	if value != "" && allIdentChars(value) {
		return value
	}
	return "\"" + strings.ReplaceAll(value, "\"", "\"\"") + "\""
}

func sqlIdentifierPath(value string) string {
	if !strings.Contains(value, ".") {
		return sqlIdentifier(value)
	}
	parts := strings.Split(value, ".")
	for i, p := range parts {
		parts[i] = sqlIdentifier(p)
	}
	return strings.Join(parts, ".")
}

func assertIdentifier(value, label string) error {
	if value == "" || !allIdentChars(value) {
		return NewError(CodeInvalidArgument,
			fmt.Sprintf("invalid %s %q: must match [A-Za-z0-9_]+", label, value))
	}
	return nil
}

func normalizeLimit(value int) (int, error) {
	if value == 0 {
		return 100, nil
	}
	if value < 0 {
		return 0, NewError(CodeInvalidArgument, "limit must be a positive integer")
	}
	return value, nil
}

func isIdentChar(r rune) bool {
	return (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
		(r >= '0' && r <= '9') || r == '_'
}

func allIdentChars(s string) bool {
	for _, r := range s {
		if !isIdentChar(r) {
			return false
		}
	}
	return true
}

// --- response parsing -------------------------------------------------

func decodeBody(body []byte) map[string]any {
	if len(body) == 0 {
		return nil
	}
	var obj map[string]any
	if err := json.Unmarshal(body, &obj); err != nil {
		return nil
	}
	return obj
}

func firstRow(body []byte) (map[string]any, uint64) {
	obj := decodeBody(body)
	if obj == nil {
		return nil, 0
	}
	affected := affectedFromMap(obj)
	rows, _ := obj["rows"].([]any)
	if len(rows) == 0 {
		if nested, ok := obj["result"].(map[string]any); ok {
			rows, _ = nested["rows"].([]any)
			if affected == 0 {
				affected = affectedFromMap(nested)
			}
		}
	}
	if len(rows) == 0 {
		return nil, affected
	}
	row, _ := rows[0].(map[string]any)
	return row, affected
}

func allRows(body []byte) []map[string]any {
	obj := decodeBody(body)
	if obj == nil {
		return nil
	}
	raw, ok := obj["rows"].([]any)
	if !ok {
		if nested, ok := obj["result"].(map[string]any); ok {
			raw, _ = nested["rows"].([]any)
		}
	}
	out := make([]map[string]any, 0, len(raw))
	for _, r := range raw {
		if m, ok := r.(map[string]any); ok {
			out = append(out, m)
		}
	}
	return out
}

func affectedFromBody(body []byte) uint64 {
	obj := decodeBody(body)
	if obj == nil {
		return 0
	}
	if n := affectedFromMap(obj); n > 0 {
		return n
	}
	if nested, ok := obj["result"].(map[string]any); ok {
		return affectedFromMap(nested)
	}
	return 0
}

func ridString(value any) (string, bool) {
	switch v := value.(type) {
	case string:
		return v, true
	case float64:
		return strconv.FormatFloat(v, 'f', -1, 64), true
	case json.Number:
		return v.String(), true
	case int64:
		return strconv.FormatInt(v, 10), true
	case uint64:
		return strconv.FormatUint(v, 10), true
	}
	return "", false
}
