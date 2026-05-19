CREATE TABLE events (id INTEGER, user_id TEXT, ts BIGINT);
INSERT INTO events (id, user_id, ts) VALUES (1, 'u1', 100);
INSERT INTO events (id, user_id, ts) VALUES (2, 'u1', 100);
INSERT INTO events (id, user_id, ts) VALUES (3, 'u1', 300);
SELECT id, DENSE_RANK() OVER (PARTITION BY user_id ORDER BY ts) AS drk FROM events ORDER BY id;
