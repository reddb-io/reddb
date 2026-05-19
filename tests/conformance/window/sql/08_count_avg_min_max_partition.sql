CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (1, 'u1', 100, 10);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (2, 'u1', 200, 20);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (3, 'u1', 300, 30);
SELECT id, COUNT(*) OVER (PARTITION BY user_id) AS c, AVG(amount) OVER (PARTITION BY user_id) AS a, MIN(amount) OVER (PARTITION BY user_id) AS mn, MAX(amount) OVER (PARTITION BY user_id) AS mx FROM purchases ORDER BY id;
