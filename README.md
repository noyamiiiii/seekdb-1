# MiniVectorDB

赛题一“轻量向量数据库”的 Rust 实现。数据库核心使用标准库，自行实现 SQL 执行、堆页、事务日志及精确向量检索，不依赖外部数据库或向量索引引擎。

## 构建与运行

```sh
docker build -t minivectordb .
docker run --rm -i --network none --mount type=bind,source="$PWD/data",target=/data minivectordb
```

提前创建主机 `data` 目录。镜像在 Linux x86_64 上从源码编译，固定 Rust 1.88、Debian bookworm 的基础镜像摘要，并执行 `cargo test --release --locked --offline`。JSON 通信仅使用允许的通用序列化库 serde_json，锁定依赖并随仓库 vendor 源码提供，构建无需下载 crate。首次获取基础镜像需要网络，运行无需网络，不监听端口。构建网络无法访问 Docker Hub 时，可通过 `RUST_IMAGE` 和 `RUNTIME_IMAGE` 构建参数使用提供相同摘要的镜像站。

本地运行：`cargo run --release --locked --offline -- --data-dir ./data`。兼容 `--data` / `-d`。默认协议严格遵循官方 [STDIO_PROTOCOL.md](STDIO_PROTOCOL.md)：主动输出 `{"type":"ready"}`，接收逐行 execute/shutdown JSON，逐请求响应并 flush；不要加 `-t`。仅手工调试可显式传入 `--text` 使用 SQL 文本终端，正式镜像默认使用 JSONL。

官方新增的交互检查工具原样保存在 `protocol_check/`，每次收到 ready 后才发送请求，包含全部公开样例、正常关闭和同目录重启：

```sh
python3 protocol_check/run.py --image minivectordb --output /tmp/minivectordb-public-check
```

## SQL 与事务

- 类型：完整 `i64` 范围的 `BIGINT`、按 Unicode 字符计数的 `VARCHAR(n)`、固定维度且仅接受有限 `float32` 的 `VECTOR(n)`。每表一个 BIGINT 主键，最多一列向量。
- DDL：`CREATE TABLE`、`DROP TABLE [IF EXISTS]`、`TRUNCATE`、`CREATE VECTOR INDEX ... ON ...`、`DROP INDEX ... ON ...`。
- DML：多行 `INSERT`、`UPDATE`、`DELETE`；查询支持投影及列别名、比较、带括号的 `AND`、`IN`、`BETWEEN`、排序、`LIMIT`、`L2_DISTANCE`。过滤在 TopK 选取之前执行，距离相同按主键升序，输出距离为欧氏距离。
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

普通记录追加写入，删除保留墓碑和原始槽号，避免撤回事务时引用失效。超过单页容量的记录使用链式溢出页。每表最多保留 16 个事务脏页，其他页溢写到 `.txn`。提交时把页镜像按 256 KiB 有界批次写入 WAL，最后追加提交标记并做一次 `fdatasync`；小批次合并为一次顺序写，大事务不会让日志缓冲随数据量增长。恢复按完整提交标记处理，忽略尾部未完成日志；页重放和重复恢复保持幂等。主键映射为内存 `HashMap<pk, RID>`，在 WAL 恢复完成后从持久化记录重建。

向量索引采用精确 flat scan。编码记录与行定位信息合计最多缓存 8 MiB（按实际分配容量计），超过限制直接流式扫描堆文件。TopK 结果直接从缓存解码，避免逐条重读堆页。最多保留 32 个过滤条件的精确候选集合，候选集合另有 1 MiB 上限；所有数据及事务可见性修改都会清除缓存。

建缓存时顺带把向量压成每维 1 字节的 7 位仿射码（表级 `min` 与 `step`），大表无过滤查询先用 AVX2 整数核扫描这份四分之一大小的编码副本，按「分桶计数取分位 + 精确复算」得到候选。压缩只作下界预筛：两侧共用同一映射，因此任一行的真实距离与编码距离之差不超过 `sqrt(维数) * (step/2 + 查询侧重建误差)`；截止值逐轮放宽，直到所有被排除行的编码距离减去该误差仍大于当前第 K 名，才认定 TopK 成立，候选永远回到原始 `f32` 字节用 `f64` 复算。输出距离、主键并列顺序与全精确扫描逐位一致，过滤查询、小表和无法证明的边界情况继续走全精确路径。TopK 使用紧凑距离堆，支持运行时检测的 AVX2/FMA 和标量回退。

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

## 本机基准

当前分支在 WSL2（i9-14900HX，P 核，数据目录在 `/tmp`）上连续 5 次的实测范围与中位数：

| 项目 | 数值 |
| --- | --- |
| 写 10,000 行（100 批） | 405–442 ms（4.05–4.42 ms/批） |
| 1,000 次 Top10 | 55.3–61.8 ms（0.060 ms/次） |
| 其中无过滤 | 32.6–34.0 ms（0.065–0.068 ms/次） |
| 其中有过滤 | 22.3–28.2 ms（0.045–0.056 ms/次） |
| 峰值 RSS | 20 MiB |

改动前（同一台机器、同一份 `examples/bench.rs`）为：写 472–499 ms，1,000 次 Top10 共 89.8–98.8 ms，其中无过滤 72.5–80.7 ms、有过滤 17.1–17.8 ms、峰值 RSS 18 MiB。按 `E = (写 + 2 × 查询) / 3` 粗略折算，查询阶段的下降抵掉了 2 MiB 的编码副本开销。

编码副本让无过滤查询的扫描量从每行 512 字节降到 128 字节，单次内核耗时从纯 `f32` 路径的约 143 µs 降到约 37 µs（qscan 隔离计时）；剩余时间由 SQL 解析、候选复算和解码渲染分摊。写路径按「每次提交一次 `fdatasync`」设计：把 10,000 行放进一个显式事务只需 62 ms，而 100 次隐式提交要 405–442 ms，可见其中约 3.4–3.9 ms/批 是 WSL overlay 上单次 fsync 的设备延迟（预分配日志文件实测并不能缩短），不是算法开销，因此写侧不再改动。

过滤查询原先整段走 `f64` 精确扫描。现在同一份 7-bit 编码也用在过滤子集上：按缓存里的过滤索引逐行打整数分，直方图定界，再只对界内的行做 `f64` 复算，误差界与无过滤路径共用同一套证明；子集小于 probe 的两倍时仍直接走精路径，不为很小的子集多付一次扫描。同一容器（Docker，2 vCPU）内交替执行的成对 A/B，各 4 轮取中位数：1,000 次查询 53.6 → 45.4 ms，其中有过滤 23.3 → 14.4 ms；无过滤 30.4 → 30.8 ms，该路径不走预筛，差异属噪声。

历史上试过把 TopK 的两段打分缓冲合并成「原地 `select_nth` 分区」，本机快约 4%（1,000 次查询 93.9 → 90.5 ms），但原地分区会打乱分数数组与行下标的对应关系，3,000 行 × 32 维的混合过滤压测立刻返回了错误的 Top1；该改动已整体回退，只在材料里保留为一次失败实验。也试过按维度优先的 `f32` 列缓存，查询反而变慢且 RSS 涨到 23 MiB，同样回退。

同样试过把打包扫描按行区间拆给两个常驻线程：单核约 27 GB/s 的扫描在 2 vCPU 上没有翻倍，线程唤醒与结果拷贝反而更贵（1,000 次查询 46 → 73 ms，其中无过滤 30.6 → 53.8 ms），已回退。

学习材料位于 `agent/conversation.json`，仓库根目录另有同名副本；其中是真实的用户 prompt、取舍记录、实验输出与复盘，未改写学生原话。
