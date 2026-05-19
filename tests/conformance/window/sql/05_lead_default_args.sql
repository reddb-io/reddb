CREATE TABLE events (id INTEGER, user_id TEXT, ts BIGINT);
INSERT INTO events (id, user_id, ts) VALUES (1, 'u1', 100);
INSERT INTO events (id, user_id, ts) VALUES (2, 'u1', 200);
INSERT INTO events (id, user_id, ts) VALUES (3, 'u1', 300);
SELECT id, LEAD(ts) OVER (PARTITION BY user_id ORDER BY ts) AS next_ts FROM events ORDER BY id;
