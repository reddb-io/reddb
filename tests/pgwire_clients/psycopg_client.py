import os

import psycopg


port = os.environ["PGPORT"]
conn = psycopg.connect(
    f"host=127.0.0.1 port={port} user=reddb dbname=reddb sslmode=disable application_name=pgwire360-psycopg",
    autocommit=True,
)
cur = conn.cursor()
cur.execute("CREATE TABLE psy_items (id INT, name TEXT)")
cur.execute(
    "INSERT INTO psy_items (id, name) VALUES (%s::int, %s::text)",
    (1, "alice"),
    prepare=True,
)
cur.execute(
    "SELECT name FROM psy_items WHERE id = %s::int",
    (1,),
    prepare=True,
)
assert cur.fetchone() == ("alice",)
cur.execute("INSERT INTO psy_vec VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway')")
cur.execute("INSERT INTO psy_vec VECTOR (dense, content) VALUES ([0.0, 1.0], 'database')")
cur.execute(
    "SEARCH SIMILAR [1.0, 0.0] COLLECTION psy_vec LIMIT %s::int",
    (1,),
    prepare=True,
)
rows = cur.fetchall()
assert rows
cur.execute(
    "ASK %s::text STRICT OFF LIMIT 1",
    ("why did incident FDD-12313 fail?",),
    prepare=True,
)
ask_row = cur.fetchone()
ask_columns = [col.name for col in cur.description]
assert ask_columns == [
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
]
assert ask_row[0] == "mock response"
assert ask_row[8] == "openai"
assert ask_row[10] is not None
assert ask_row[11] is not None
conn.close()
