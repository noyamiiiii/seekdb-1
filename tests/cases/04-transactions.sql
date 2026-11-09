-- name: transactions

CREATE TABLE t (id BIGINT PRIMARY KEY, name VARCHAR(8));
BEGIN;
INSERT INTO t VALUES (1, 'a');
SELECT id FROM t;
ROLLBACK;
SELECT id FROM t;
BEGIN;
INSERT INTO t VALUES (2, 'b');
COMMIT;
BEGIN;
INSERT INTO t VALUES (3, 'c');
SELECT id FROM t ORDER BY id;
COMMIT;
SELECT id FROM t ORDER BY id;
COMMIT;
ROLLBACK;
BEGIN;
BEGIN;
-- expect
Query OK, 0 rows affected
Query OK, 0 rows affected
Query OK, 1 row affected
+----+
| id |
+----+
|  1 |
+----+
1 row in set
Query OK, 0 rows affected
+----+
| id |
+----+
+----+
0 rows in set
Query OK, 0 rows affected
Query OK, 1 row affected
Query OK, 0 rows affected
Query OK, 0 rows affected
Query OK, 1 row affected
+----+
| id |
+----+
|  2 |
|  3 |
+----+
2 rows in set
Query OK, 0 rows affected
+----+
| id |
+----+
|  2 |
|  3 |
+----+
2 rows in set
ERROR 1568: no transaction is open
ERROR 1568: no transaction is open
Query OK, 0 rows affected
ERROR 1567: a transaction is already open
