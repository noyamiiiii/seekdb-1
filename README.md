# MiniVectorDB

赛题一「轻量向量数据库」的实现：一个只依赖 Rust 标准库的单机数据库，通过
stdin/stdout 与 MySQL 风格测试驱动交互，数据持久化在 `/data`。

- 语言与版本：Rust 1.88（edition 2021，`rust-version = 1.75`），无第三方依赖
- 平台：Linux x86_64 容器，单进程、单客户端
- 入口：`minivectordb --data <目录>`，标准输入输出即协议通道

## 构建镜像并启动

```sh
docker build -t minivectordb .
docker run -i --rm -v "$PWD/data:/data" minivectordb
```

镜像为多阶段构建：`rust:1.88-slim-bookworm` 里 `cargo build --release --locked`，
运行镜像只带二进制。构建阶段没有任何 crate 依赖，因此不需要联网拉取索引。
运行容器使用 `-i`（保持 stdin 打开），不要用 `-d` 或 `-t`；不监听端口。

容器启动后先输出一行 `READY`，之后每读入一条以 `;` 结尾的语句就输出一个响应块：

| 响应 | 含义 |
| --- | --- |
| `Query OK, N rows affected` | DML 成功 |
| `Query OK, 0 rows affected` | DDL / `SET` / `BEGIN` / `ROLLBACK` 成功 |
| `+---+` 表格 + `N rows in set` | 查询结果 |
| `ERROR 1146: table 't' does not exist` | 失败，连接继续可用 |
| `BYE` | 收到 `exit;` / `quit;` / `\q` 后关闭 |

## 数据目录

`/data` 下只有三类文件，均为崩溃安全写入：

| 文件 | 内容 |
| --- | --- |
| `catalog.bin` | 表名与表结构，DDL 时通过临时文件原子替换 |
| `<表名>.heap` | 行数据，64 KiB 定长页的追加式堆文件 |
| `wal.log` | 提交日志，只保留「最后一个 checkpoint 之后」的页镜像 |

## SQL 支持范围

| 类别 | 支持 |
| --- | --- |
| DDL | `CREATE TABLE`（`BIGINT PRIMARY KEY` / `BIGINT` / `VARCHAR(n)` / `VECTOR(n)`）、`DROP TABLE [IF EXISTS]`、`TRUNCATE [TABLE]`、`CREATE VECTOR INDEX`、`DROP INDEX ... ON ...` |
| 写入 | `INSERT ... VALUES`（可带列名、多行）、`UPDATE ... SET ... WHERE`、`DELETE [FROM ... WHERE]` |
| 查询 | `SELECT` 列 / `*` / 常量（无 `FROM`，如 `SELECT 1 AS one`）、`WHERE`（`AND`、`= < <= > >=`、`IN (...)`、`BETWEEN ... AND ...`）、`ORDER BY`（单列，`ASC`/`DESC`，并列按主键升序）、`LIMIT`、`L2_DISTANCE(列, '[…]') [AS 别名]` |
| 事务 | `BEGIN` / `START TRANSACTION` / `COMMIT` / `ROLLBACK`，自动提交模式下每条语句一个事务 |
| 元数据 | `SHOW TABLES`、`DESCRIBE 表` / `DESC 表` / `SHOW COLUMNS FROM 表` |
| 其它 | `SET name = value`、`SET NAMES utf8mb4`、`USE 库`、`CREATE`/`DROP DATABASE`（接受并忽略）；命令行未知选项（含 `--mode exact\|ann`）一律忽略，不会因多余参数启动失败 |

不支持：`JOIN`、聚合、子查询、`NULL`、`ALTER TABLE`、外键、嵌套事务、更新主键。
`DROP TABLE`/`CREATE TABLE`/`TRUNCATE` 在显式事务内会先隐式提交（与 MySQL 一致）。
不论驱动传 `exact` 还是 `ann` 模式，检索都走精确扫描，因此两种模式下的 TopK 结果都精确。

### 错误码

| 码 | 场景 | 码 | 场景 |
| --- | --- | --- | --- |
| 1050 | 表已存在 | 1146 | 表不存在 |
| 1054 | 未知列 | 1136 | 值个数与列数不符 |
| 1060 | 列重复 | 1062 | 主键重复 |
| 1064 | 语法错误 | 1235 | 不支持的操作（更新主键等）|
| 1264 | 数值越界 | 1366 | 向量维度/取值非法 |
| 1406 | `VARCHAR` 超长 | 1567 | 事务重复开启 |
| 1568 | 无开启的事务 | 1105 | I/O 等内部错误 |

`VARCHAR(n)` 按 Unicode 字符计数；向量必须是有限 `float32`（拒绝 `NaN`/`Infinity`
与维度不符）；字符串写入后原样返回。

## 从空目录开始的操作示例

```sh
docker run -i --rm -v "$PWD/data:/data" minivectordb <<'SQL'
CREATE TABLE docs (id BIGINT PRIMARY KEY, tag VARCHAR(8), emb VECTOR(3));
INSERT INTO docs VALUES (1, 'a', '[1,0,0]'), (2, 'b', '[0,1,0]'), (3, 'c', '[1,1,0]');
SELECT id, L2_DISTANCE(emb, '[0,0,1]') AS d FROM docs ORDER BY d LIMIT 2;
BEGIN;
INSERT INTO docs VALUES (4, 'd', '[2,2,0]');
SELECT id FROM docs ORDER BY id;
ROLLBACK;
SELECT id FROM docs ORDER BY id;
exit;
SQL
```

重启用同一个 `/data`，已提交的数据仍在：

```sh
docker run -i --rm -v "$PWD/data:/data" minivectordb <<'SQL'
SELECT id FROM docs ORDER BY id;
exit;
SQL
```

## 设计与验证记录

### 记录布局

每张表一个堆文件，64 KiB 一页。页头 8 字节（页号、槽数、空闲区末位置），
随后是 4 字节槽表（offset + length，最高位是墓碑标记），记录从页尾向前生长。
记录先按列的编码顺序存定长/变长字段，主键索引是内存 `BTreeMap<pk → RID>`，
启动时从堆重建，所以索引永远可以从持久化数据恢复。删除只写墓碑，页尾记录
按顺序扫描填满；checkpoint 时做一次崩溃安全的 compaction（写 `*.heap.tmp`、
fsync、rename）。

### 向量检索

精确扫描 + 有界堆 TopK：`ORDER BY L2_DISTANCE(col, q) LIMIT k` 时只保留 k 条
（`BinaryHeap<Ranked>`，内存 O(k)），并列距离按主键升序，先过滤后取 K。
没有显式 `ORDER BY` 时不保证顺序（符合「按集合比较」的约定）。距离用 f64
累加、结果打印 6 位小数。

热点路径用原始字节直扫：过滤只涉及普通列、排序是距离且带 `LIMIT` 时，
跳过行解码与分配，直接按列的定长/变长布局读向量算距离，只有进 TopK 的行
才解码（`RawFilter` + 距离快路径）。

### 事务与提交持久化

事务用 undo log：`INSERT` 记 RID 与主键，回滚时写墓碑并删索引项；`DELETE`
记 RID，回滚时复活。语句失败自动回滚，显式事务内查询可见自己未提交的修改。

提交流程（一次 fsync）：把本次事务修改的页镜像追加进 `wal.log`
（`Begin`、每页一条、`Commit`），`fsync` 一次即为提交点；然后把这批页写回
堆文件（不 fsync，因为日志已覆盖它们）。`wal.log` 累计超过 4 MiB 或进程
正常退出时做 checkpoint：堆文件 fsync + 截断日志。恢复时重放最后一个完整
`Commit` 之前的页镜像，日志里没有提交记录的事务视为不存在，因此不会出现
「写了一半」的语句。

### 本地测试与性能

`cargo test --release`：102 个测试全绿，分布为 70 个库内单测、6 个协议测试、
14 个执行器测试、9 个存储测试、2 个崩溃测试、1 个 SQL 用例集测试。

- 崩溃测试：真 `SIGKILL` 服务进程（含「发出去还没读到响应」的语句），重启后
  断言已确认的语句全部存在、没有半条语句、没有重复主键。
- 用例集：`tests/cases/*.sql` 是 mysqltest 风格的文件，语句与期望输出写在同一
  文件里，`tests/cases.rs` 用真实二进制跑完整会话逐行比对（覆盖 DDL/DML、
  向量检索并列、错误码、事务、类型边界）。重新录制用 `sh tests/cases/record.sh`。

本机 benchmark（`cargo run --release --example bench`，10 000 行 × 128 维、
1 000 次 TopK 查询、100 行一批提交，Docker/WSL2 上）：

| 指标 | 优化前 | 优化后 |
| --- | --- | --- |
| 写 10 000 行 | 1 631 ms | 767 ms |
| 1 000 次查询 | 2 235 ms | 924 ms |

优化来自三处：提交只 fsync 一次、checkpoint 延迟到 4 MiB 日志之后、检索走
原始字节快路径并对距离做 8 路展开。

### 已知限制

- 单客户端、无 `JOIN`/聚合/`NULL`，主键不可更新
- 近似索引未实现：提交配置声明 `exact` 模式；`CREATE VECTOR INDEX` 只做语法与
  参数校验（表/列存在、列是 `VECTOR`），检索始终走精确扫描，Recall@10 = 1.0
- 全表扫描按页读取，单次查询的 I/O 与行数线性相关；页面不常驻内存，以换取
  更低的内存峰值
- DDL 与事务的交互按 MySQL 的「隐式提交」处理，事务内没有 DDL 的原子性