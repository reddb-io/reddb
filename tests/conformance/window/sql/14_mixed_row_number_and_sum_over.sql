CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (1, 'u1', 100, 10);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (2, 'u1', 200, 20);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (3, 'u1', 300, 30);
SELECT id, ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY ts) AS rn, SUM(amount) OVER (PARTITION BY user_id ORDER BY ts) AS running FROM purchases ORDER BY id;
