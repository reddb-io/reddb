// Package redserver carries an end-to-end smoke that spawns the real `red`
// engine binary and drives it via the Go driver. It is opt-in:
//
//   - Skipped by default and when RED_SKIP_SMOKE=1.
//   - When RED_SMOKE=1 is set, the test compiles + spawns the engine. Set
//     RED_BIN=<path> to point at a pre-built binary; otherwise the test runs
//     `cargo build --bin red --release` from the repo root, which can be slow.
package redserver

import (
	"bytes"
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	reddb "github.com/reddb-io/reddb-go"
)

func TestSmoke_AgainstRealServer(t *testing.T) {
	if os.Getenv("RED_SKIP_SMOKE") == "1" {
		t.Skip("RED_SKIP_SMOKE=1 set")
	}
	if os.Getenv("RED_SMOKE") != "1" {
		t.Skip("set RED_SMOKE=1 to enable the engine smoke; off by default")
	}

	bin := os.Getenv("RED_BIN")
	if bin == "" {
		t.Skip("set RED_BIN=/path/to/red to run the engine smoke")
	}
	if _, err := os.Stat(bin); err != nil {
		t.Skipf("RED_BIN %q not found: %v", bin, err)
	}

	tmpdir := t.TempDir()
	dataPath := filepath.Join(tmpdir, "data.db")

	port, err := pickFreePort()
	if err != nil {
		t.Fatalf("pick free port: %v", err)
	}

	var logs bytes.Buffer
	cmd := exec.Command(bin, "server",
		"--path", dataPath,
		"--bind", fmt.Sprintf("127.0.0.1:%d", port),
	)
	cmd.Stdout = &logs
	cmd.Stderr = &logs
	if err := cmd.Start(); err != nil {
		t.Fatalf("start engine: %v", err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	})

	uri := fmt.Sprintf("red://127.0.0.1:%d", port)

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	c, err := waitForConnect(ctx, uri)
	if err != nil {
		t.Fatalf("connect: %v\nserver logs:\n%s", err, logs.String())
	}
	defer c.Close()

	if err := c.Ping(ctx); err != nil {
		t.Errorf("ping: %v", err)
	}
	if _, err := c.Query(ctx, "SELECT 1"); err != nil {
		t.Errorf("query SELECT 1: %v", err)
	}
	if _, err := c.Exec(ctx, "CREATE TABLE go_params (id INT, name TEXT)"); err != nil {
		t.Errorf("create go_params: %v", err)
	}
	inserted, err := c.Exec(ctx,
		"INSERT INTO go_params (id, name) VALUES ($1, $2)",
		int64(42), "Ada")
	if err != nil {
		t.Errorf("parameterized exec insert: %v", err)
	} else if inserted.RowsAffected() != 1 {
		t.Errorf("parameterized exec affected = %d", inserted.RowsAffected())
	}
	body, err := c.Query(ctx, "SELECT name FROM go_params WHERE id = $1", int64(42))
	if err != nil {
		t.Errorf("parameterized query select: %v", err)
	} else if !strings.Contains(string(body), "Ada") {
		t.Errorf("parameterized query body missing row: %s", body)
	}

	row := map[string]any{"id": "k1", "v": "hello"}
	if err := c.Insert(ctx, "smoke_kv", row); err != nil {
		t.Errorf("insert: %v", err)
	}
	if _, err := c.Get(ctx, "smoke_kv", "k1"); err != nil {
		t.Errorf("get: %v", err)
	}
	if err := c.Delete(ctx, "smoke_kv", "k1"); err != nil {
		t.Errorf("delete: %v", err)
	}
}

func pickFreePort() (int, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0, err
	}
	defer ln.Close()
	return ln.Addr().(*net.TCPAddr).Port, nil
}

func waitForConnect(ctx context.Context, uri string) (reddb.Conn, error) {
	var lastErr error
	for {
		if err := ctx.Err(); err != nil {
			if lastErr != nil {
				return nil, lastErr
			}
			return nil, err
		}
		c, err := reddb.Connect(ctx, uri)
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
