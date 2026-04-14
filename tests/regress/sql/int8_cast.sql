CREATE TABLE metrics (age INT8);
INSERT INTO metrics (age) VALUES (123456);
SELECT CAST(age AS BIGINT) AS agebig FROM metrics;
