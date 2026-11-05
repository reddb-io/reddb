# SQL quotation rules

SQL uses double quotes for identifiers and single quotes for text values:

```sql
CREATE TABLE "select" ("key name" INT PRIMARY KEY, "text value" TEXT);
INSERT INTO "select" ("key name", "text value") VALUES (1, 'hello');
SELECT "text value" FROM "select" WHERE "key name" = 1;
```

An identifier may name a table, column, alias, or constraint. Double an embedded
quote: `"a""b"` names the column `a"b`. Ordinary unquoted identifiers continue to
work. Field paths keep their dots outside the quotes, for example
`"profile name"."city"`. Literal dots inside a quoted column or alias are rejected
because the query AST cannot distinguish them from field paths.
Quoted keywords such as `"CASE"` and `"current_user"` are column names,
not expressions with special behavior.

For text, double an embedded single quote: `'it''s ready'`. Prefer query
parameters for application values; parameter encoding is unchanged:

```sql
INSERT INTO "select" ("key name", "text value") VALUES ($1, $2);
```

Parameters represent values, not identifiers. Use a driver identifier helper
when its supported name rules fit, or correctly delimit names in SQL passed to
the driver's query method. Never interpolate unescaped application input.

## Migration from double-quoted SQL strings

This is a breaking grammar change with no legacy toggle. Replace SQL text
literals such as `WHERE owner = "alice"` with `WHERE owner = 'alice'`, or bind
`alice` as a parameter. `SELECT "title"` now reads the column named `title`;
`SELECT 'title'` returns the text `title`.

Update application SQL, fixtures, saved queries, views, and migration scripts
before adopting this engine version. A wire/SDK upgrade alone cannot rewrite
SQL strings stored in an application. Existing binary parameters and HTTP JSON
request bodies keep their encodings.

## JSON keeps double-quoted strings

JSON objects and array values retain JSON quoting:

```sql
CREATE TABLE payloads (id INT, body JSON);
INSERT INTO payloads (id, body)
VALUES (1, {"title":"hello","tags":["one","two"]});
```

Do not replace double quotes inside JSON request bodies, stored JSON values, or
inline JSON literals. They delimit JSON keys and text, not SQL identifiers.

## Dots in collection names

Collection names may contain dots. `app.audit.events`, `"app"."audit"."events"`,
and `"app.audit.events"` all refer to the same collection. Dots are part of its
catalog name; creating or dropping it does not create or drop a collection
named `app`. The `red` namespace, including names starting with `red.`, remains
reserved for the system. Empty segments and wildcard segments are not valid
collection names (the existing `DROP COLLECTION app.*` wildcard is a separate
operation).

```sql
CREATE TABLE app.audit.events (id INT PRIMARY KEY, message TEXT);
INSERT INTO "app.audit.events" (id, message) VALUES (1, 'created');
SELECT e.message FROM app.audit.events e WHERE e.id = 1;
```

Use a table alias to qualify fields in queries. For KV commands whose dot also
separates the collection from its key, quote the entire collection name:

```sql
CREATE KV app.audit.cache;
KV PUT "app.audit.cache".theme = 'dark';
KV GET "app.audit.cache".theme;
KV LIST app.audit.cache;
```
