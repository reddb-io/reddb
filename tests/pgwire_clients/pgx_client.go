package main

import (
	"context"
	"fmt"
	"os"

	"github.com/jackc/pgx/v5"
)

func main() {
	ctx := context.Background()
	port := os.Getenv("PGPORT")
	cfg, err := pgx.ParseConfig(fmt.Sprintf("postgres://reddb@127.0.0.1:%s/reddb?sslmode=disable", port))
	if err != nil {
		panic(err)
	}
	cfg.RuntimeParams["application_name"] = "pgwire360-pgx"
	conn, err := pgx.ConnectConfig(ctx, cfg)
	if err != nil {
		panic(err)
	}
	defer conn.Close(ctx)

	if _, err := conn.Exec(ctx, "CREATE TABLE pgx_items (id INT, name TEXT)"); err != nil {
		panic(err)
	}
	if _, err := conn.Exec(ctx,
		"INSERT INTO pgx_items (id, name) VALUES ($1::int, $2::text)",
		int32(1), "alice",
	); err != nil {
		panic(err)
	}
	var name string
	if err := conn.QueryRow(ctx,
		"SELECT name FROM pgx_items WHERE id = $1::int",
		int32(1),
	).Scan(&name); err != nil {
		panic(err)
	}
	if name != "alice" {
		panic("unexpected select row")
	}
	if _, err := conn.Exec(ctx, "INSERT INTO pgx_vec VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway')"); err != nil {
		panic(err)
	}
	if _, err := conn.Exec(ctx, "INSERT INTO pgx_vec VECTOR (dense, content) VALUES ([0.0, 1.0], 'database')"); err != nil {
		panic(err)
	}
	rows, err := conn.Query(ctx,
		"SEARCH SIMILAR [1.0, 0.0] COLLECTION pgx_vec LIMIT $1::int",
		int32(1),
	)
	if err != nil {
		panic(err)
	}
	defer rows.Close()
	if !rows.Next() {
		panic("expected vector row")
	}
}
