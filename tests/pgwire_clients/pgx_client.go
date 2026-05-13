package main

import (
	"context"
	"fmt"
	"os"
	"strings"

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
	rows.Close()

	askRows, err := conn.Query(ctx,
		"ASK $1::text STRICT OFF LIMIT 1",
		"why did incident FDD-12313 fail?",
	)
	if err != nil {
		panic(err)
	}
	defer askRows.Close()
	fields := askRows.FieldDescriptions()
	names := make([]string, len(fields))
	for i, field := range fields {
		names[i] = string(field.Name)
	}
	expected := []string{
		"answer",
		"cache_hit",
		"citations",
		"completion_tokens",
		"cost_usd",
		"mode",
		"model",
		"prompt_tokens",
		"provider",
		"retry_count",
		"sources_flat",
		"validation",
	}
	if strings.Join(names, ",") != strings.Join(expected, ",") {
		panic(fmt.Sprintf("unexpected ASK columns: %v", names))
	}
	if !askRows.Next() {
		panic("expected ASK row")
	}
	values, err := askRows.Values()
	if err != nil {
		panic(err)
	}
	if fmt.Sprint(values[0]) != "mock response" || fmt.Sprint(values[8]) != "openai" {
		panic(fmt.Sprintf("unexpected ASK row: %v", values))
	}
}
