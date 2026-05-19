CREATE TABLE events (id INTEGER, user_id TEXT, ts BIGINT);
INSERT INTO events (id, user_id, ts) VALUES (1, 'u1', 100);
INSERT INTO events (id, user_id, ts) VALUES (2, 'u1', 200);
INSERT INTO events (id, user_id, ts) VALUES (3, 'u2', 50);
INSERT INTO events (id, user_id, ts) VALUES (4, 'u1', 150);
INSERT INTO events (id, user_id, ts) VALUES (5, 'u2', 75);
SELECT id, user_id, ts, ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY ts) AS rn FROM events ORDER BY id;
