# SQL quotation rules

SQL uses double quotes for identifiers and single quotes for text values:

```sql
CREATE TABLE "select" ("key name" INT PRIMARY KEY, "text value" TEXT);
INSERT INTO "select" ("key name", "text value") VALUES (1, 'hello');
SELECT "text value" FROM "select" WHERE "key name" = 1;
```

An identifier may name a table, column, alias, or constraint. Double an embedded
quote: `"a""b"` names the column `a"b`. Ordinary unquoted identifiers continue to
work. Paths keep their dots outside the quotes, for example
`"profile name"."city"`. Literal dots inside a single quoted identifier are
currently rejected because the query AST cannot distinguish them from paths.
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
