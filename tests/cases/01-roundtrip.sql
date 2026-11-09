-- name: ddl/dml roundtrip

SET NAMES utf8mb4;
CREATE TABLE items (id BIGINT PRIMARY KEY, name VARCHAR(8), emb VECTOR(2));
INSERT INTO items (id, name, emb) VALUES (1, 'alpha', '[1,2]'), (2, 'beta', '[3,4]');
INSERT INTO items VALUES (3, 'gamma', '[5,6]');
SELECT id, name, emb FROM items ORDER BY id;
UPDATE items SET name = 'delta' WHERE id = 2;
SELECT id, name FROM items WHERE id = 2;
DELETE FROM items WHERE id = 3;
SELECT id FROM items ORDER BY id;
-- expect
Query OK, 0 rows affected
Query OK, 0 rows affected
Query OK, 2 rows affected
Query OK, 1 row affected
+----+-------+-------+
| id | name  | emb   |
+----+-------+-------+
|  1 | alpha | [1,2] |
|  2 | beta  | [3,4] |
|  3 | gamma | [5,6] |
+----+-------+-------+
3 rows in set
Query OK, 1 row affected
+----+-------+
| id | name  |
+----+-------+
|  2 | delta |
+----+-------+
1 row in set
Query OK, 1 row affected
+----+
| id |
+----+
|  1 |
|  2 |
+----+
2 rows in set
