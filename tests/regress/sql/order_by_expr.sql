CREATE TABLE users (name TEXT);
INSERT INTO users (name) VALUES ('betty');
INSERT INTO users (name) VALUES ('alice');
SELECT UPPER(name) AS upper FROM users ORDER BY UPPER(name);
