-- name: types and formatting

CREATE TABLE t (id BIGINT PRIMARY KEY, tag VARCHAR(6), v VECTOR(2));
INSERT INTO t VALUES (-9223372036854775808, '中文', '[1.5,-2.25]');
INSERT INTO t VALUES (9223372036854775807, 'ok', '[0,0]');
SELECT id, tag, v FROM t ORDER BY id LIMIT 1;
SELECT id, tag, v FROM t ORDER BY id DESC LIMIT 1;
SELECT id FROM t WHERE id = -9223372036854775808;
SELECT v FROM t WHERE id < 0;
UPDATE t SET v = '[3,4]' WHERE id > 0;
SELECT L2_DISTANCE(v, '[3,4]') FROM t WHERE id > 0;
-- expect
Query OK, 0 rows affected
Query OK, 1 row affected
Query OK, 1 row affected
+----------------------+-----+-------------+
| id                   | tag | v           |
+----------------------+-----+-------------+
| -9223372036854775808 | 中文  | [1.5,-2.25] |
+----------------------+-----+-------------+
1 row in set
+---------------------+-----+-------+
| id                  | tag | v     |
+---------------------+-----+-------+
| 9223372036854775807 | ok  | [0,0] |
+---------------------+-----+-------+
1 row in set
+----------------------+
| id                   |
+----------------------+
| -9223372036854775808 |
+----------------------+
1 row in set
+-------------+
| v           |
+-------------+
| [1.5,-2.25] |
+-------------+
1 row in set
Query OK, 1 row affected
+----------------+
| l2_distance(v) |
+----------------+
|       0.000000 |
+----------------+
1 row in set
