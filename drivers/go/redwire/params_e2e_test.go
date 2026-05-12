package redwire

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"strings"
	"testing"
)

// Drives the QueryWithParams happy path against a fake server that advertises
// FeatureParams + responds with a Result. Covers the int / text / null /
// vector mix called out in #363 acceptance.
func TestQuery_WithParams_RoundTrip(t *testing.T) {
	srv, cli := newFakeServerPair(t)
	defer srv.close()

	srv.wg.Add(1)
	go func() {
		defer srv.wg.Done()
		srv.consumeMagic()
		hello := srv.readFrame()
		ack, _ := json.Marshal(map[string]any{
			"auth":     "anonymous",
			"features": float64(FeatureParams),
		})
		srv.writeFrame(NewFrame(KindHelloAck, hello.CorrelationID, ack))
		_ = srv.readFrame()
		ok, _ := json.Marshal(map[string]any{
			"session_id": "sess",
			"features":   float64(FeatureParams),
		})
		srv.writeFrame(NewFrame(KindAuthOk, 1, ok))

		q := srv.readFrame()
		if q.Kind != KindQueryWithParams {
			t.Errorf("expected QueryWithParams (0x28), got 0x%02x", q.Kind)
		}
		// Sanity-check payload header: sql + 4 params.
		payload := q.Payload
		if len(payload) < 8 {
			t.Errorf("payload too short: %d", len(payload))
		}
		sqlLen := binary.LittleEndian.Uint32(payload[0:4])
		if string(payload[4:4+sqlLen]) != "SELECT $1, $2, $3, $4" {
			t.Errorf("sql mismatch: %q", payload[4:4+sqlLen])
		}
		paramCount := binary.LittleEndian.Uint32(payload[4+sqlLen : 4+sqlLen+4])
		if paramCount != 4 {
			t.Errorf("param_count: %d", paramCount)
		}
		srv.writeFrame(NewFrame(KindResult, q.CorrelationID,
			[]byte(`{"records":[]}`)))
	}()

	c, err := connectViaPipe(t, cli, ConnOptions{Auth: AuthCreds{Method: AuthAnonymous}})
	if err != nil {
		t.Fatalf("handshake: %v", err)
	}
	if !c.SupportsParams() {
		t.Fatal("client should see FeatureParams advertised")
	}
	body, err := c.Query(context.Background(), "SELECT $1, $2, $3, $4",
		int64(42), "alice", nil, []float32{1, 2, 3})
	if err != nil {
		t.Fatalf("query: %v", err)
	}
	if string(body) != `{"records":[]}` {
		t.Errorf("body: %q", body)
	}
}

// When the server omits FEATURE_PARAMS, parameterized calls must error
// rather than silently downgrade or send raw `$N` literals.
func TestQuery_WithParams_UnsupportedServer(t *testing.T) {
	srv, cli := newFakeServerPair(t)
	defer srv.close()

	srv.wg.Add(1)
	go func() {
		defer srv.wg.Done()
		srv.consumeMagic()
		hello := srv.readFrame()
		ack, _ := json.Marshal(map[string]any{"auth": "anonymous"}) // no features
		srv.writeFrame(NewFrame(KindHelloAck, hello.CorrelationID, ack))
		_ = srv.readFrame()
		ok, _ := json.Marshal(map[string]any{"session_id": "s"})
		srv.writeFrame(NewFrame(KindAuthOk, 1, ok))
		// Server should never see a Query frame — driver must short-circuit
		// before writing. Reading the next frame would block, which is fine.
	}()

	c, err := connectViaPipe(t, cli, ConnOptions{Auth: AuthCreds{Method: AuthAnonymous}})
	if err != nil {
		t.Fatalf("handshake: %v", err)
	}
	if c.SupportsParams() {
		t.Fatal("client must not see FeatureParams")
	}
	_, err = c.Query(context.Background(), "SELECT $1", int64(1))
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "FEATURE_PARAMS") {
		t.Errorf("expected FEATURE_PARAMS in error, got %v", err)
	}
}

// Empty params keeps emitting the legacy Query frame even when the server
// supports parameterized queries — guards the byte-for-byte backwards path.
func TestQuery_NoParams_EmitsLegacyQueryFrame(t *testing.T) {
	srv, cli := newFakeServerPair(t)
	defer srv.close()

	srv.wg.Add(1)
	go func() {
		defer srv.wg.Done()
		srv.consumeMagic()
		hello := srv.readFrame()
		ack, _ := json.Marshal(map[string]any{
			"auth":     "anonymous",
			"features": float64(FeatureParams),
		})
		srv.writeFrame(NewFrame(KindHelloAck, hello.CorrelationID, ack))
		_ = srv.readFrame()
		ok, _ := json.Marshal(map[string]any{"session_id": "s"})
		srv.writeFrame(NewFrame(KindAuthOk, 1, ok))

		q := srv.readFrame()
		if q.Kind != KindQuery {
			t.Errorf("expected KindQuery (0x01), got 0x%02x", q.Kind)
		}
		srv.writeFrame(NewFrame(KindResult, q.CorrelationID, []byte("{}")))
	}()

	c, err := connectViaPipe(t, cli, ConnOptions{Auth: AuthCreds{Method: AuthAnonymous}})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := c.Query(context.Background(), "SELECT 1"); err != nil {
		t.Fatalf("query: %v", err)
	}
}
