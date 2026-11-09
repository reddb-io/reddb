package reddb

import (
	"context"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestEmbeddedNativeABI(t *testing.T) {
	if os.Getenv("REDDB_EMBEDDED_SMOKE") != "1" {
		t.Skip("set REDDB_EMBEDDED_SMOKE=1 with REDDB_FFI_LIBRARY to run")
	}
	ctx := context.Background()
	memory, err := Connect(ctx, "memory://")
	if err != nil {
		t.Fatalf("open memory: %v", err)
	}
	if _, err := memory.Exec(ctx, "CREATE TABLE go_embedded (id INTEGER, name TEXT)"); err != nil {
		t.Fatalf("create: %v", err)
	}
	if _, err := memory.Exec(
		ctx,
		"INSERT INTO go_embedded (id, name) VALUES ($1, $2)",
		int64(7),
		"embedded",
	); err != nil {
		t.Fatalf("insert: %v", err)
	}
	rows, err := memory.Query(ctx, "SELECT name FROM go_embedded WHERE id = $1", int64(7))
	if err != nil || !strings.Contains(string(rows), "embedded") {
		t.Fatalf("select: %v body=%s", err, rows)
	}
	if err := memory.Close(); err != nil {
		t.Fatalf("close memory: %v", err)
	}

	directory := t.TempDir()
	database := filepath.Join(directory, "driver-contract.rdb")
	uri := (&url.URL{Scheme: "file", Path: filepath.ToSlash(database)}).String()
	first, err := Connect(ctx, uri)
	if err != nil {
		t.Fatalf("open file: %v", err)
	}
	if _, err := first.Exec(ctx, "CREATE TABLE durable (id INTEGER, name TEXT)"); err != nil {
		t.Fatalf("create durable: %v", err)
	}
	if _, err := first.Exec(ctx, "INSERT INTO durable (id, name) VALUES (1, 'persisted')"); err != nil {
		t.Fatalf("insert durable: %v", err)
	}
	if second, err := Connect(ctx, uri); err == nil {
		_ = second.Close()
		t.Fatal("second writer unexpectedly opened")
	}
	if err := first.Close(); err != nil {
		t.Fatalf("close first: %v", err)
	}

	second, err := Connect(ctx, uri)
	if err != nil {
		t.Fatalf("reopen file: %v", err)
	}
	rows, err = second.Query(ctx, "SELECT name FROM durable WHERE id = 1")
	if err != nil || !strings.Contains(string(rows), "persisted") {
		t.Fatalf("reopen select: %v body=%s", err, rows)
	}
	if err := second.Close(); err != nil {
		t.Fatalf("close second: %v", err)
	}

	entries, err := os.ReadDir(directory)
	if err != nil {
		t.Fatalf("list artifacts: %v", err)
	}
	if len(entries) != 1 || entries[0].Name() != filepath.Base(database) {
		t.Fatalf("expected one database artifact, got %v", entries)
	}
}
