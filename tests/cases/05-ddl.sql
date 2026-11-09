-- name: ddl

CREATE TABLE t (id BIGINT PRIMARY KEY, v VECTOR(2));
CREATE VECTOR INDEX idx ON t (v);
CREATE TABLE t (id BIGINT PRIMARY KEY, v VECTOR(2));
INSERT INTO t VALUES (1, '[0,0]');
DROP TABLE t;
SELECT id FROM t;
CREATE TABLE u (id BIGINT PRIMARY KEY, tag VARCHAR(4), v VECTOR(2));
INSERT INTO u VALUES (1, 'a', '[0,0]');
BEGIN;
DROP TABLE u;
INSERT INTO u VALUES (2, 'b', '[1,1]');
COMMIT;
-- expect
Query OK, 0 rows affected
Query OK, 0 rows affected
ERROR 1050: table 't' already exists
Query OK, 1 row affected
Query OK, 0 rows affected
ERROR 1146: table 't' does not exist
Query OK, 0 rows affected
Query OK, 1 row affected
Query OK, 0 rows affected
Query OK, 0 rows affected
ERROR 1146: table 'u' does not exist
ERROR 1568: no transaction is open
