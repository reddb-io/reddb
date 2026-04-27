// redgo-smoke is a manual smoke runnable against a live RedDB server. It does
// not run as part of `go test ./...`; invoke it explicitly with
//
//	go run ./cmd/redgo-smoke red://localhost:5050
package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"time"

	reddb "github.com/forattini-dev/reddb-go"
)

func main() {
	flag.Parse()
	args := flag.Args()
	if len(args) == 0 {
		log.Fatal("usage: redgo-smoke <uri>  (e.g. red://localhost:5050)")
	}
	uri := args[0]

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	c, err := reddb.Connect(ctx, uri)
	if err != nil {
		log.Fatalf("connect: %v", err)
	}
	defer c.Close()

	if err := c.Ping(ctx); err != nil {
		log.Fatalf("ping: %v", err)
	}
	fmt.Fprintln(os.Stderr, "ping ok")

	body, err := c.Query(ctx, "SELECT 1")
	if err != nil {
		log.Fatalf("query: %v", err)
	}
	fmt.Fprintf(os.Stderr, "query result: %s\n", string(body))

	row := map[string]any{"name": "smoke-go", "ts": time.Now().Unix()}
	if err := c.Insert(ctx, "smoke_kv", row); err != nil {
		log.Fatalf("insert: %v", err)
	}
	fmt.Fprintln(os.Stderr, "insert ok")
}
