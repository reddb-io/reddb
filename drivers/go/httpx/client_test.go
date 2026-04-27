package httpx

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// fakeServer mounts a tiny mux that pretends to be the RedDB HTTP surface.
func fakeServer(t *testing.T) (*httptest.Server, *recorder) {
	t.Helper()
	rec := &recorder{}
	mux := http.NewServeMux()
	mux.HandleFunc("/auth/login", func(w http.ResponseWriter, r *http.Request) {
		rec.path = r.URL.Path
		_, _ = io.ReadAll(r.Body)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":     true,
			"result": map[string]any{"token": "tok-abc"},
		})
	})
	mux.HandleFunc("/admin/health", func(w http.ResponseWriter, r *http.Request) {
		rec.path = r.URL.Path
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":     true,
			"result": map[string]any{"status": "ok"},
		})
	})
	mux.HandleFunc("/query", func(w http.ResponseWriter, r *http.Request) {
		rec.path = r.URL.Path
		rec.auth = r.Header.Get("authorization")
		body, _ := io.ReadAll(r.Body)
		var obj map[string]any
		_ = json.Unmarshal(body, &obj)
		rec.lastBody = obj
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":     true,
			"result": map[string]any{"records": []any{}, "echo": obj["query"]},
		})
	})
	mux.HandleFunc("/collections/", func(w http.ResponseWriter, r *http.Request) {
		rec.path = r.URL.Path
		rec.method = r.Method
		body, _ := io.ReadAll(r.Body)
		_ = body
		switch r.Method {
		case http.MethodGet:
			_ = json.NewEncoder(w).Encode(map[string]any{
				"ok":     true,
				"result": map[string]any{"id": strings.TrimPrefix(r.URL.Path, "/collections/")},
			})
		case http.MethodDelete:
			_ = json.NewEncoder(w).Encode(map[string]any{
				"ok":     true,
				"result": map[string]any{"affected": 1},
			})
		default:
			_ = json.NewEncoder(w).Encode(map[string]any{
				"ok":     true,
				"result": map[string]any{"affected": 1},
			})
		}
	})
	mux.HandleFunc("/error", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(500)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":    false,
			"error": "boom",
		})
	})
	mux.HandleFunc("/raw500", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(500)
		_, _ = w.Write([]byte("plain text failure"))
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv, rec
}

type recorder struct {
	path     string
	method   string
	auth     string
	lastBody map[string]any
}

func TestClient_LoginStoresToken(t *testing.T) {
	srv, _ := fakeServer(t)
	c, err := NewClient(Options{BaseURL: srv.URL})
	if err != nil {
		t.Fatal(err)
	}
	res, err := c.Login(context.Background(), "u", "p")
	if err != nil {
		t.Fatal(err)
	}
	if res.Token != "tok-abc" {
		t.Errorf("token = %q", res.Token)
	}
	if c.Token() != "tok-abc" {
		t.Errorf("client did not retain token")
	}
}

func TestClient_QuerySendsAuthHeader(t *testing.T) {
	srv, rec := fakeServer(t)
	c, _ := NewClient(Options{BaseURL: srv.URL, Token: "preset"})
	if _, err := c.Query(context.Background(), "SELECT 1"); err != nil {
		t.Fatal(err)
	}
	if rec.auth != "Bearer preset" {
		t.Errorf("auth header = %q", rec.auth)
	}
	if rec.lastBody["query"] != "SELECT 1" {
		t.Errorf("body = %v", rec.lastBody)
	}
}

func TestClient_Health(t *testing.T) {
	srv, _ := fakeServer(t)
	c, _ := NewClient(Options{BaseURL: srv.URL})
	out, err := c.Health(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	obj, ok := out.(map[string]any)
	if !ok || obj["status"] != "ok" {
		t.Errorf("got %v", out)
	}
}

func TestClient_Insert_GetDelete(t *testing.T) {
	srv, rec := fakeServer(t)
	c, _ := NewClient(Options{BaseURL: srv.URL})

	if _, err := c.Insert(context.Background(), "users", map[string]any{"name": "alice"}); err != nil {
		t.Fatal(err)
	}
	if !strings.HasSuffix(rec.path, "/users/rows") {
		t.Errorf("insert path %q", rec.path)
	}
	if rec.method != http.MethodPost {
		t.Errorf("insert method %q", rec.method)
	}

	out, err := c.Get(context.Background(), "users", "abc-123")
	if err != nil {
		t.Fatal(err)
	}
	obj, ok := out.(map[string]any)
	if !ok || !strings.HasSuffix(obj["id"].(string), "users/abc-123") {
		t.Errorf("get returned %v", out)
	}

	if _, err := c.Delete(context.Background(), "users", "abc-123"); err != nil {
		t.Fatal(err)
	}
	if rec.method != http.MethodDelete {
		t.Errorf("delete method %q", rec.method)
	}
}

func TestClient_BulkInsert(t *testing.T) {
	srv, _ := fakeServer(t)
	c, _ := NewClient(Options{BaseURL: srv.URL})
	_, err := c.BulkInsert(context.Background(), "users", []any{
		map[string]any{"name": "alice"},
		map[string]any{"name": "bob"},
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestClient_ServerErrorEnvelope(t *testing.T) {
	srv, _ := fakeServer(t)
	c, _ := NewClient(Options{BaseURL: srv.URL})
	_, err := c.do(context.Background(), http.MethodGet, "/error", nil)
	if err != nil {
		t.Fatal(err)
	}
	// Use parseEnvelope directly so we observe the error path.
	resp, _ := c.do(context.Background(), http.MethodGet, "/error", nil)
	_, perr := parseEnvelope(resp)
	if perr == nil {
		t.Fatal("expected parseEnvelope error")
	}
	if !strings.Contains(perr.Error(), "boom") {
		t.Errorf("got %v", perr)
	}
}

func TestClient_Raw500(t *testing.T) {
	srv, _ := fakeServer(t)
	c, _ := NewClient(Options{BaseURL: srv.URL})
	_, err := c.Query(context.Background(), "ok") // exercise unrelated path
	if err != nil {
		t.Fatal(err)
	}
	resp, _ := c.do(context.Background(), http.MethodGet, "/raw500", nil)
	if _, err := parseEnvelope(resp); err == nil {
		t.Error("expected error for 500")
	}
}

func TestClient_Scan(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/scan", func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		var obj map[string]any
		_ = json.Unmarshal(body, &obj)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":     true,
			"result": map[string]any{"records": []any{obj}},
		})
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()
	c, _ := NewClient(Options{BaseURL: srv.URL})
	out, err := c.Scan(context.Background(), map[string]any{"collection": "x"})
	if err != nil {
		t.Fatal(err)
	}
	if out == nil {
		t.Fatal("nil result")
	}
}
