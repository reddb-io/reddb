CREATE TABLE pairs (name TEXT, alias TEXT);
INSERT INTO pairs (name, alias) VALUES ('same', 'same');
INSERT INTO pairs (name, alias) VALUES ('diff', 'other');
SELECT name FROM pairs WHERE name = alias ORDER BY name;
