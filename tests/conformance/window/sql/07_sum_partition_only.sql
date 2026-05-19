CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (1, 'u1', 100, 10);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (2, 'u1', 200, 20);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (3, 'u1', 300, 30);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (4, 'u2', 100, 7);
INSERT INTO purchases (id, user_id, ts, amount) VALUES (5, 'u2', 200, 9);
SELECT id, user_id, SUM(amount) OVER (PARTITION BY user_id) AS total FROM purchases ORDER BY id;
