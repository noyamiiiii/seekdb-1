# MiniVectorDB stdio protocol (v0.1)

This document is the contract between the server binary and the evaluation
driver. Everything below is fixed by the project (the task statement leaves
framing details open); changing it means changing both sides.

## 1. Process lifecycle

* The server runs in the **foreground**: no daemon, no socket, no fork.
* It reads SQL from **stdin** and writes results to **stdout**; diagnostics go to
  stderr. stdout is never used for logging.
* All persistent data lives under the data directory, `--data <dir>`
  (default `/data`).
* Start-up: recover the data directory and rebuild vector indexes, **then** print
  exactly one line:

  ```
  READY
  ```

  The driver sends the first statement only after it has read this line.
* Shutdown: `exit;` / `quit;` / `\q` (also `exit 1`) answers `BYE` and exits 0.
  End of input without `exit` is a clean shutdown: the server rolls back any open
  transaction, flushes, and exits 0. A crash (non-zero exit) is a failure.

## 2. Statement framing

* Statements are separated by `;` at top level. A `;` inside a string literal
  (`'...'`, with `''` and `\` escapes) or inside a comment does not terminate.
* Statements may span any number of lines; leading/trailing whitespace is not
  significant.
* Comments: `-- ` to end of line, `#` to end of line, `/* ... */` (no nesting).
* An empty statement (`;;`) is ignored and produces no block.
* At most one open transaction per session; the session survives statement
  errors.

## 3. Response blocks

Each statement produces exactly **one** block, written immediately and flushed
(it is read line by line). Blocks never interleave, and no blank line separates
them.

### 3.1 Result set

```
+----+-------+
| id | name  |
+----+-------+
|  1 | alice |
| 20 | bo    |
+----+-------+
2 rows in set
```

* Column width is the widest of the header and all cells, measured in Unicode
  scalar values.
* Every cell is padded with one space on each side; `BIGINT` and distance
  (`REAL`) cells are right aligned, all other cells left aligned.
* An empty result set still prints the header and the `+--+` borders, then
  `0 rows in set` and no data lines.
* Trailing summary: `1 row in set`, `N rows in set`, `0 rows in set`.

### 3.2 Status

```
Query OK, 1 row affected
```

`row affected` for exactly one row, `rows affected` otherwise; DDL and
transaction control report `0 rows affected`.

### 3.3 Errors

```
ERROR <code>: <message>
```

One line: newlines in messages are replaced by spaces. The session stays alive
and the next statement is processed normally.

### 3.4 Goodbye

```
BYE
```

Written once, in answer to `exit;`.

## 4. Value rendering

| Type      | Rendering |
|-----------|-----------|
| `BIGINT`  | decimal, no separators or sign for zero (`0`, `-12`) |
| `VARCHAR` | raw characters, no quotes, UTF-8 |
| `VECTOR`  | `[x,y,z]` — `,` separated, no spaces, each element in the shortest form that round-trips to the stored `f32` (`[1,1.5,-2]`) |
| distance  | fixed six decimals (`1.500000`, `0.000000`) |

Distance columns produced by `L2_DISTANCE` are `REAL` and therefore right
aligned.

## 5. Error codes

Codes follow MySQL's public numbering where an equivalent condition exists;
the transaction pair at the end is this project's own mapping.

| Code | Name | When |
|------|------|------|
| 1050 | `ER_TABLE_EXISTS_ERROR` | `CREATE TABLE` on an existing name |
| 1052 | `ER_NON_UNIQ_ERROR` | column name matches more than one source |
| 1054 | `ER_BAD_FIELD_ERROR` | unknown column |
| 1062 | `ER_DUP_ENTRY` | duplicate primary key |
| 1064 | `ER_PARSE_ERROR` | lexical/grammatical error |
| 1105 | `ER_UNKNOWN_ERROR` | generic failure |
| 1146 | `ER_NO_SUCH_TABLE` | unknown table |
| 1235 | `ER_NOT_SUPPORTED_YET` | valid SQL outside the supported subset |
| 1264 | `ER_WARN_DATA_OUT_OF_RANGE` | numeric value outside the type |
| 1366 | `ER_TRUNCATED_WRONG_VALUE` | value not convertible to the column type |
| 1406 | `ER_DATA_TOO_LONG` | `VARCHAR(n)` value longer than `n` |
| 1567 | `ER_NESTED_TRANSACTION` | `BEGIN` while a transaction is open |
| 1568 | `ER_NO_ACTIVE_TRANSACTION` | `COMMIT`/`ROLLBACK` with no transaction |

Message text is free-form but must not span lines.

## 6. Session state

* `SET vector_mode = 'exact' | 'ann'` selects the search strategy for subsequent
  `SELECT` statements in the session; the default is `exact`.
* Statements are executed one at a time; there is no pipelining requirement
  beyond the per-block flush.
* `exit` inside a transaction rolls back and still answers `BYE`.

## 7. Persistence

The data directory is the single source of truth: killing the process and
restarting it with the same `--data` must expose every committed row and table
definition. Uncommitted work must be discarded on restart.

## 8. Compatibility notes

* Keywords are case-insensitive; identifiers keep their case for display.
* One statement per line is *not* required; the driver may send several
  statements in one chunk.
* No statement returns more than one result set.