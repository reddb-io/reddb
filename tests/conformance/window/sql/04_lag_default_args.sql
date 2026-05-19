CREATE TABLE events (id INTEGER, user_id TEXT, ts BIGINT);
INSERT INTO events (id, user_id, ts) VALUES (1, 'u1', 100);
INSERT INTO events (id, user_id, ts) VALUES (2, 'u1', 200);
INSERT INTO events (id, user_id, ts) VALUES (3, 'u1', 300);
INSERT INTO events (id, user_id, ts) VALUES (4, 'u2', 50);
SELECT id, LAG(ts) OVER (PARTITION BY user_id ORDER BY ts) AS prev_ts FROM events ORDER BY id;
