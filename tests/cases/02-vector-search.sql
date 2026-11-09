-- name: vector search top-k

CREATE TABLE vec (id BIGINT PRIMARY KEY, tag VARCHAR(4), v VECTOR(3));
INSERT INTO vec VALUES (1, 'a', '[1,0,0]'), (2, 'b', '[0,1,0]'), (3, 'c', '[1,1,0]'), (4, 'd', '[2,2,0]'), (5, 'e', '[3,3,0]');
SELECT id, L2_DISTANCE(v, '[0,0,0]') AS d FROM vec WHERE id >= 2 ORDER BY d ASC LIMIT 3;
SELECT id FROM vec ORDER BY L2_DISTANCE(v, '[9,9,0]') DESC LIMIT 2;
SELECT id FROM vec ORDER BY tag ASC LIMIT 2;
SELECT id, tag FROM vec WHERE tag > 'b' ORDER BY tag;
-- expect
Query OK, 0 rows affected
Query OK, 5 rows affected
+----+----------+
| id | d        |
+----+----------+
|  2 | 1.000000 |
|  3 | 1.414214 |
|  4 | 2.828427 |
+----+----------+
3 rows in set
+----+
| id |
+----+
|  1 |
|  2 |
+----+
2 rows in set
+----+
| id |
+----+
|  1 |
|  2 |
+----+
2 rows in set
+----+-----+
| id | tag |
+----+-----+
|  3 | c   |
|  4 | d   |
|  5 | e   |
+----+-----+
3 rows in set
