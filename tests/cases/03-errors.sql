-- name: errors

CREATE TABLE t (id BIGINT PRIMARY KEY, name VARCHAR(4), v VECTOR(2));
INSERT INTO t VALUES (1, 'abcd', '[1,2]');
INSERT INTO t VALUES (1, 'x', '[1,2]');
INSERT INTO t VALUES (2, 'abcde', '[1,2]');
INSERT INTO t VALUES (2, 'x', '[1,2,3]');
INSERT INTO t VALUES (2, 'x', '[NaN,1]');
SELECT * FROM missing;
SELECT nope FROM t;
INSERT INTO t VALUES (3, 'x', '[1,2]';
UPDATE t SET id = 9 WHERE id = 1;
INSERT INTO t (nope) VALUES (1);
-- expect
Query OK, 0 rows affected
Query OK, 1 row affected
ERROR 1062: duplicate primary key 1 in table 't'
ERROR 1406: data too long for column 'name': 5 characters, limit 4
ERROR 1366: column 'v' expects a vector of 2 elements, got 3
ERROR 1366: '[NaN,1]' is not a valid vector literal for column 'v'
ERROR 1146: table 'missing' does not exist
ERROR 1054: Unknown column 'nope'
ERROR 1064: expected ')' (at character 38)
ERROR 1235: updating the primary key is not supported
ERROR 1054: Unknown column 'nope'
