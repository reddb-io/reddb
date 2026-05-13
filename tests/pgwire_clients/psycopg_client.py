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
conn.close()
