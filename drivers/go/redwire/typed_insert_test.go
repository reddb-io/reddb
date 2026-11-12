package redwire

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"testing"
)

func TestEncodeTypedInsertMatchesRustGoldenBytes(t *testing.T) {
	payload, err := EncodeTypedInsert("users", []Field{
		{Name: "name", Value: "Ada"},
		{Name: "age", Value: int64(37)},
	})
	if err != nil {
		t.Fatal(err)
	}
	const want = "0105007573657273020004006e616d6504030000004164610300616765022500000000000000"
	if got := hex.EncodeToString(payload); got != want {
		t.Fatalf("payload bytes\n got: %s\nwant: %s", got, want)
	}
}

func TestTypedInsertRoundTrip(t *testing.T) {
	srv, cli := newFakeServerPair(t)
	defer srv.close()

	srv.wg.Add(1)
	go func() {
		defer srv.wg.Done()
		srv.consumeMagic()
		hello := srv.readFrame()
		ack, _ := json.Marshal(map[string]any{
			"auth": "anonymous", "features": float64(FeatureTypedMutations),
		})
		srv.writeFrame(NewFrame(KindHelloAck, hello.CorrelationID, ack))
		_ = srv.readFrame()
		ok, _ := json.Marshal(map[string]any{
			"session_id": "typed", "features": float64(FeatureTypedMutations),
		})
		srv.writeFrame(NewFrame(KindAuthOk, 1, ok))

		request := srv.readFrame()
		if request.Kind != KindTypedInsert {
			t.Errorf("expected TypedInsert (0x33), got 0x%02x", request.Kind)
			return
		}
		payload := request.Payload
		if payload[0] != typedInsertVersion {
			t.Errorf("version = %d", payload[0])
			return
		}
		collectionLen := int(binary.LittleEndian.Uint16(payload[1:3]))
		if got := string(payload[3 : 3+collectionLen]); got != "users" {
			t.Errorf("collection = %q", got)
		}
		fieldCount := binary.LittleEndian.Uint16(payload[3+collectionLen : 5+collectionLen])
		if fieldCount != 2 {
			t.Errorf("field count = %d", fieldCount)
		}
		srv.writeFrame(NewFrame(
			KindBulkOk,
			request.CorrelationID,
			[]byte(`{"affected":1,"ids":["9007199254740993"]}`),
		))
	}()

	c, err := connectViaPipe(t, cli, ConnOptions{Auth: AuthCreds{Method: AuthAnonymous}})
	if err != nil {
		t.Fatalf("handshake: %v", err)
	}
	if !c.SupportsTypedMutations() {
		t.Fatal("typed mutations feature should be visible")
	}
	result, err := c.TypedInsert(context.Background(), "users", []Field{
		{Name: "name", Value: "Ada"},
		{Name: "age", Value: int64(37)},
	})
	if err != nil {
		t.Fatalf("typed insert: %v", err)
	}
	if result.Affected != 1 || len(result.IDs) != 1 || result.IDs[0] != "9007199254740993" {
		t.Fatalf("result = %#v", result)
	}
}

func TestTypedInsertRequiresNegotiatedFeature(t *testing.T) {
	srv, cli := newFakeServerPair(t)
	defer srv.close()

	srv.wg.Add(1)
	go func() {
		defer srv.wg.Done()
		srv.consumeMagic()
		hello := srv.readFrame()
		ack, _ := json.Marshal(map[string]any{"auth": "anonymous"})
		srv.writeFrame(NewFrame(KindHelloAck, hello.CorrelationID, ack))
		_ = srv.readFrame()
		ok, _ := json.Marshal(map[string]any{"session_id": "old"})
		srv.writeFrame(NewFrame(KindAuthOk, 1, ok))

		ping := srv.readFrame()
		if ping.Kind != KindPing {
			t.Errorf("typed insert wrote a frame before feature check: 0x%02x", ping.Kind)
			return
		}
		srv.writeFrame(NewFrame(KindPong, ping.CorrelationID, nil))
	}()

	c, err := connectViaPipe(t, cli, ConnOptions{Auth: AuthCreds{Method: AuthAnonymous}})
	if err != nil {
		t.Fatalf("handshake: %v", err)
	}
	_, err = c.TypedInsert(context.Background(), "users", nil)
	if !errors.Is(err, ErrTypedMutationsUnsupported) {
		t.Fatalf("expected ErrTypedMutationsUnsupported, got %v", err)
	}
	if err := c.Ping(context.Background()); err != nil {
		t.Fatalf("connection should remain usable: %v", err)
	}
}

func TestEncodeTypedInsertRejectsAmbiguousFields(t *testing.T) {
	_, err := EncodeTypedInsert("users", []Field{
		{Name: "id", Value: 1},
		{Name: "id", Value: 2},
	})
	if err == nil {
		t.Fatal("duplicate field must fail")
	}
}
