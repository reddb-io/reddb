CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (1, 'u1', 100, 10);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (2, 'u1', 200, NULL);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (3, 'u1', 300, 30);
SELECT id, COUNT(*) OVER (PARTITION BY user_id) AS c_all, COUNT(amount) OVER (PARTITION BY user_id) AS c_amt, SUM(amount) OVER (PARTITION BY user_id) AS s_amt FROM purchases ORDER BY id;
