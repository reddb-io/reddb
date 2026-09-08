-- Double quotes identify names; single quotes delimit SQL text.
CREATE TABLE "select" ("key name" INT PRIMARY KEY, "text value" TEXT, body JSON);
INSERT INTO "select" ("key name", "text value", body)
VALUES (1, 'it''s ready', {"title":"hello","tags":["one","two"]});
SELECT "text value", body FROM "select" WHERE "key name" = 1;
SHOW CREATE TABLE "select";
