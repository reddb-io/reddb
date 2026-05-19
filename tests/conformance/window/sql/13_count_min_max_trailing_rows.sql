CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (1, 'u1', 100, 10);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (2, 'u1', 200, 40);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (3, 'u1', 300, 20);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (4, 'u1', 400, 30);
SELECT id, COUNT(amount) OVER (PARTITION BY user_id ORDER BY ts ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS c2, MIN(amount) OVER (PARTITION BY user_id ORDER BY ts ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS mn2, MAX(amount) OVER (PARTITION BY user_id ORDER BY ts ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS mx2 FROM purchases ORDER BY id;
