# MiniVectorDB

赛题一“轻量向量数据库”的 Rust 实现。只使用标准库，自行实现 SQL 执行、堆页、事务日志及精确向量检索，不依赖外部数据库或向量索引引擎。

## 构建与运行

```sh
docker build -t minivectordb .
docker run --rm -i --network none --mount type=bind,source="$PWD/data",target=/data minivectordb
```

提前创建主机 `data` 目录。镜像在 Linux x86_64 上从源码编译，固定 Rust 1.88、Debian bookworm 的基础镜像摘要，并执行 `cargo test --release --locked --offline`。项目无第三方 crate。首次获取基础镜像需要网络，运行无需网络，不监听端口。构建网络无法访问 Docker Hub 时，可通过 `RUST_IMAGE` 和 `RUNTIME_IMAGE` 构建参数使用提供相同摘要的镜像站。

本地运行：`cargo run --release --locked --offline -- --data-dir ./data`。兼容 `--data` / `-d`。使用标准输入输出交互，启动输出 `READY`；不要加 `-t`。协议详见 [docs/protocol.md](docs/protocol.md)。公开题包尚未提供冻结版驱动，因此仍需以该驱动的最终协议检查为准。

## SQL 与事务

- 类型：完整 `i64` 范围的 `BIGINT`、按 Unicode 字符计数的 `VARCHAR(n)`、固定维度且仅接受有限 `float32` 的 `VECTOR(n)`。每表一个 BIGINT 主键，最多一列向量。
- DDL：`CREATE TABLE`、`DROP TABLE [IF EXISTS]`、`TRUNCATE`、`CREATE VECTOR INDEX ... ON ...`、`DROP INDEX ... ON ...`。
- DML：多行 `INSERT`、`UPDATE`、`DELETE`；查询支持投影、比较、`AND`、`IN`、`BETWEEN`、排序、`LIMIT`、`L2_DISTANCE`。过滤在 TopK 选取之前执行，距离相同按主键升序，输出距离为欧氏距离。
- 事务：`BEGIN` / `START TRANSACTION`、`COMMIT`、`ROLLBACK`，读到自己的修改。失败语句撤回自身修改，保留显式事务中更早的成功语句；正常退出撤回尚未提交的事务。成功提交前同步日志。
- 会话：`SET vector_mode = 'exact'` 或 `'ann'`、`SET NAMES utf8mb4`。两种向量模式均使用精确检索。未知变量和数据库命名空间命令返回不支持。
- 辅助：`SHOW TABLES`、`DESCRIBE`、常量 `SELECT`。不支持聚合、JOIN、子查询、NULL、外键、嵌套事务和更新主键。

```sql
CREATE TABLE docs(id BIGINT PRIMARY KEY, tag VARCHAR(8), emb VECTOR(3));
INSERT INTO docs VALUES (1, '甲', '[1,0,0]'), (2, '乙', '[0,1,0]');
CREATE VECTOR INDEX docs_emb ON docs(emb);
SELECT id, L2_DISTANCE(emb, '[0,0,1]') AS d FROM docs ORDER BY d LIMIT 2;
BEGIN;
UPDATE docs SET tag = '丙' WHERE id = 1;
ROLLBACK;
```

## 存储与恢复

所有引擎文件在指定的数据目录内：

| 文件 | 用途 |
| --- | --- |
| `catalog.bin` | 表结构；通过临时文件、同步和原子替换保存 |
| `<table>.heap` | 64 KiB 定长堆页；普通页为 8 字节页头和每槽 8 字节的槽表 |
| `<table>.txn` | 未提交页的临时溢写文件；启动时丢弃，不视为已提交数据 |
| `<table>.<index>.vidx` | 持久化向量索引定义 |
| `wal.log` | 已提交事务的页镜像，恢复时先重放再重建主键映射 |

普通记录追加写入，删除保留墓碑和原始槽号，避免撤回事务时引用失效。超过单页容量的记录使用链式溢出页。每表最多保留 16 个事务脏页，其他页溢写到 `.txn`，提交时流式记录到 WAL。恢复按完整提交标记处理，忽略尾部未完成日志；页重放和重复恢复保持幂等。主键映射为内存 `HashMap<pk, RID>`，在 WAL 恢复完成后从持久化记录重建。

向量索引采用精确 flat scan。最多缓存 8 MiB 编码记录，超过限制直接流式扫描堆文件。修改、撤回和重启不会复用过期缓存。TopK 比较平方距离，输出时计算平方根；支持运行时检测的 AVX2/FMA 双精度累加路径和标量回退。

数据文件可以超过内存，但主键映射、事务撤回元数据和完整无 LIMIT 查询结果仍随其规模增长；不宣称支持任意数据量下的恒定内存。当前未实现 ANN 近似索引。

## 自测

```sh
cargo test --release --locked --offline
cargo build --release --locked --offline --all-targets
python3 tools/official_cases.py target/release/minivectordb mysqltest
python3 tools/adversarial.py target/release/minivectordb
cargo run --release --locked --offline --example bench
```

公开 v4 题包原样位于 `mysqltest/`。自测适配器将 SQL 发给实际标准输入输出进程并检查公开 JSON 结果。额外测试覆盖失败语句的原子性、退出回滚、大记录、索引可见性、Unicode、完整 BIGINT 范围和 SIMD 尾部。

Linux 专用的对抗测试使用独立 Python 参考模型，检查混合事务和过滤 TopK，限制进程地址空间来验证大于内存的数据与事务，以及强制终止和反复恢复。`tools/docker_driver.cmd` 可在 Windows 上将公开用例发给最终镜像，每个容器使用 2 核、128 MiB、禁用网络。

`examples/bench.rs` 使用 10,000 行、128 维、1,000 次 Top10，其中一半含普通过滤；其进程 RSS、I/O 和耗时仅供本地比较，不能替代官方 cgroup 统计、隐藏测试、最终排名或分数。
