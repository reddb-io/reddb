// Package redserver carries an end-to-end smoke that spawns the real `red`
// engine binary and drives it via the Go driver. It is opt-in:
//
//   - Skipped by default and when RED_SKIP_SMOKE=1.
//   - When RED_SMOKE=1 is set, the test compiles + spawns the engine. Set
//     RED_BIN=<path> to point at a pre-built binary; otherwise the test runs
//     `cargo build --bin red --release` from the repo root, which can be slow.
package redserver

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	reddb "github.com/forattini-dev/reddb-go"
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
	dataPath := filepath.Join(tmpdir, "data")
	_ = os.MkdirAll(dataPath, 0o755)

	// Bind on :0 so the OS picks a free port; parse it from stdout.
	cmd := exec.Command(bin, "server",
		"--path", dataPath,
		"--bind", "127.0.0.1:0",
	)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatalf("start engine: %v", err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	})

	port, err := waitForPort(io.MultiReader(stdout, stderr), 15*time.Second)
	if err != nil {
		t.Fatalf("read engine port: %v", err)
	}
	uri := fmt.Sprintf("red://127.0.0.1:%d", port)

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	c, err := reddb.Connect(ctx, uri)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer c.Close()

	if err := c.Ping(ctx); err != nil {
		t.Errorf("ping: %v", err)
	}
	if _, err := c.Query(ctx, "SELECT 1"); err != nil {
		t.Errorf("query SELECT 1: %v", err)
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

// waitForPort scans the engine's combined stdout/stderr for a "listening on
// 127.0.0.1:<port>" line. Times out if nothing matches in time.
func waitForPort(r io.Reader, timeout time.Duration) (int, error) {
	deadline := time.Now().Add(timeout)
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 1<<16), 1<<20)
	re := regexp.MustCompile(`(?:listening on|bound to|bind=)\s*(?:127\.0\.0\.1|0\.0\.0\.0|\[::\]|\[::1\]):(\d+)`)
	for scanner.Scan() {
		if time.Now().After(deadline) {
			return 0, fmt.Errorf("timeout reading engine output")
		}
		line := scanner.Text()
		if m := re.FindStringSubmatch(line); m != nil {
			return strconv.Atoi(m[1])
		}
		// Cheaper alternative: any line containing :NNNN that's a likely port.
		if strings.Contains(line, "redwire") || strings.Contains(line, "0.0.0.0:") {
			if alt := regexp.MustCompile(`:(\d{4,5})\b`).FindStringSubmatch(line); alt != nil {
				if n, err := strconv.Atoi(alt[1]); err == nil {
					return n, nil
				}
			}
		}
	}
	if err := scanner.Err(); err != nil {
		return 0, err
	}
	return 0, fmt.Errorf("engine output ended before port was logged")
}
