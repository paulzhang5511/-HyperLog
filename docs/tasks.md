# Task Breakdown: Hyper Log

| 项目 | 内容 |
| --- | --- |
| 上游文档 | `docs/prd.md` → `docs/spec.md`（事实来源） → `docs/plan.md`（方案与顺序） |
| 本文档 | **逐任务拆分**（Phase 3: TASKS） |
| 状态 | 待评审，评审通过后按 T01→T20 顺序执行 |
| 创建日期 | 2026-09-01 |

**通用约束（每个任务都适用）**
- 完成即跑：`cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`
- 性能相关任务必须在 `--release` 下测量，`dev` 构建的正则性能慢 10~50 倍，不具备参考价值
- 改动 `core/` 公开 API 或 §7 契约前，先改 `spec.md`
- 命令清单见 `spec.md` §5；目录结构见 `spec.md` §6

---

## Phase 0：基础骨架

### T01: 依赖与构建配置

**Description:** 建立 `Cargo.toml` 完整依赖与 release profile，确认 `egui`/`eframe` 0.36.1 在当前工具链（rustc 1.95，edition 2024）下可编译。这是整个项目的**高风险前置**——若版本不可用，后续所有 API 假设都要重来，所以必须先验证再动手。

**Acceptance criteria:**
- [ ] `Cargo.toml` 含 `spec.md` §4 全部依赖，edition 保持 **2024**
- [ ] `[profile.release]` 含 `lto = "thin"` / `codegen-units = 1` / `panic = "abort"` / `strip = true`
- [ ] `cargo build --release` **首次编译通过**（依赖全部解析成功）
- [ ] 若实际解析版本与 PRD 指定不一致，已回写 `spec.md` §4 版本表并记录理由
- [ ] `.gitignore` 覆盖 `/target`、`/tmp`、`*.log`（大样本不入库）

**Verification:**
- [ ] `cargo build --release` 退出码 0
- [ ] `cargo tree --depth 1` 输出的实际版本已与 `spec.md` §4 比对
- [ ] 手工：记录首次编译耗时（后续任务的基线参考）

**Dependencies:** None
**Files:** `Cargo.toml`, `.gitignore`, `spec.md` §4
**Scope:** XS (1–2 files)

---

### T02: 应用骨架与窗口启动

**Description:** 建立 §6 约定的模块骨架与最小 egui 应用：配置原生窗口（标题、初始尺寸、最小尺寸），初始化 `env_logger`，三块面板（顶部工具栏 / 中央日志区 / 底部状态栏）留占位。`main.rs` 保持在 60 行以内，只负责启动。

**Acceptance criteria:**
- [ ] `src/` 下存在 `main.rs` / `app.rs` / `ui/{mod,toolbar,log_view,status_bar}.rs` / `highlight.rs` / `core/{mod,indexer,search,export}.rs` / `util.rs`（可为空模块）
- [ ] 窗口标题 "Hyper Log"，初始尺寸 ≥ 1200×800，最小尺寸 ≥ 800×600
- [ ] 三块面板渲染占位文本，窗口可缩放、最大化、关闭
- [ ] `env_logger` 初始化，`RUST_LOG=info` 时可看到启动日志
- [ ] `main.rs` < 60 行
- [ ] 退出时进程干净退出（无残留线程）

**Verification:**
- [ ] `cargo run --release` 启动，窗口正常显示
- [ ] `RUST_LOG=debug cargo run` 能看到初始化日志
- [ ] 手工：拖动窗口缩放、最小化还原、Cmd/Ctrl+Q 退出均无异常
- [ ] `cargo clippy --all-targets -- -D warnings` 无告警（空模块注意 `unused` 警告）

**Dependencies:** T01
**Files:** `src/main.rs`, `src/app.rs`, `src/ui/mod.rs`, `src/ui/toolbar.rs`, `src/ui/log_view.rs`, `src/ui/status_bar.rs`, `src/core/mod.rs`, `src/util.rs`
**Scope:** S (2 files 实质内容 + 骨架)

---

### T03: 测试脚手架与 CI

**Description:** 建立测试基础设施：`scripts/gen_log.sh` 合成日志生成器、`tests/fixtures/` 七类边界样本、GitHub Actions 双平台 CI。fixtures 是后续 T04 单测的输入，必须先于 T04 就位。

**Acceptance criteria:**
- [ ] `scripts/gen_log.sh <path> <lines>` 生成 timestamp + level + 随机正文的合成日志，平均行长 ~100 B
- [ ] `tests/fixtures/` 含 7 个样本，各自特征明确（见 `spec.md` §9.3）：`empty.log`(0 B) / `no_trailing_newline.log` / `crlf.log` / `bom.log` / `invalid_utf8.log` / `levels.log`(含 `errors=3` 干扰串) / `long_line.log`(单行 1 MB) / `multi/`(3 文件)
- [ ] `.github/workflows/ci.yml`：matrix `[macos-latest, windows-latest] × stable`，步骤含 `fmt --check` / `clippy -D warnings` / `test --all`
- [ ] `tests/perf_test.rs` 骨架就绪，大文件用例标注 `#[ignore]`

**Verification:**
- [ ] `bash scripts/gen_log.sh /tmp/smoke.log 1000 && wc -l /tmp/smoke.log` 输出 1000
- [ ] `ls -la tests/fixtures/` 七类样本齐全，`empty.log` 大小确认为 0
- [ ] 推送后 CI 双平台全绿（或本地 `act` 验证）
- [ ] `cargo test --all` 通过（此时应无实质测试，验证脚手架不报错）

**Dependencies:** T01
**Files:** `scripts/gen_log.sh`, `tests/fixtures/*`, `tests/perf_test.rs`, `.github/workflows/ci.yml`
**Scope:** M (多文件，但均为数据与配置)

---

> ### ✅ Checkpoint A：基础就绪
> - [ ] `cargo build --release` 通过，**egui 版本可用性已验证**
> - [ ] 空白窗口可启动 / 缩放 / 关闭
> - [ ] CI 双平台全绿，fixtures 就位
> - [ ] **人工确认**：实际依赖版本已回写 `spec.md` §4

---

## Phase 1：索引核心

### T04: `LogFileIndex` 内存映射与行索引

**Description:** 实现 `spec.md` §7.1 的 `LogFileIndex`：mmap 映射、行偏移索引、按行取字节/文本。必须覆盖 §7.1 边界规则表的全部 7 类输入——PRD 的实现在这里有 5 个缺陷（空文件 panic、空文件被当作 1 行、CRLF 未剥离、BOM 未跳过、无效 UTF-8 丢行），全部要在这一任务里修掉并用单测钉死。

**Acceptance criteria:**
- [ ] `open()` 处理空文件（返回 `IndexError::Empty`，**不 panic**）、超 16 GiB（返回 `TooLarge`）、权限不足（返回 `Io`）
- [ ] `line_count()`：空文件 = 0；`"abc"` = 1；`"a\nb\n"` = 2；`"\n\n"` = 2
- [ ] `line()` 剥离行尾 `\r`（CRLF）与 `\n`；跳过首部 UTF-8 BOM
- [ ] 含无效 UTF-8 的行返回 `Cow::Owned`（U+FFFD 替换），**不返回 None**（不丢行）
- [ ] `line_bytes()` 返回原始字节切片（供导出使用，不做 UTF-8 转换）
- [ ] `index_memory_bytes()` 返回 `line_offsets.len() * 8`
- [ ] 唯一的 `unsafe`（`Mmap::map` 调用点）带 `// SAFETY:` 注释
- [ ] `core/indexer.rs` 内**无** `unwrap()` / `expect()` / `panic!()`

**Verification:**
- [ ] `cargo test --all -- indexer` 通过，7 类边界样本各有独立测试用例
- [ ] proptest 属性测试：随机字节 + 随机换行 → 不变式 1（行覆盖完整性）与不变式 2（偏移严格递增）成立
- [ ] `cargo clippy --all-targets -- -D warnings` 无告警
- [ ] 手工：`scripts/gen_log.sh /tmp/bench_1gb.log 10_000_000`，打点 `open()` 耗时，**P1 要求 < 2.0 s**
- [ ] 手工：`index_memory_bytes()` 对应 1000 万行应 ≤ 80 MB，**P3 要求 ≤ 8 B/行**

**Dependencies:** T03（fixtures）
**Files:** `src/core/indexer.rs`, `src/core/mod.rs`, `tests/indexer_test.rs`
**Scope:** M (3 files)

---

### T05: `FileSet` 全局行寻址

**Description:** 实现 `spec.md` §7.2 的 `FileSet`：把"文件 i 的第 j 行"统一映射为 `0..total_lines` 的全局行号。这是修复 PRD 多文件渲染缺陷（D1）的关键抽象，也是后续检索结果渲染的公共底座。

**Acceptance criteria:**
- [ ] `cumulative` 前缀和数组正确，`total_lines()` = 各文件行数之和
- [ ] `resolve(global_idx)` 在边界处正确（每个文件的首行/末行），越界返回 `None`
- [ ] `line(global_idx)` 跨文件连续：第 i 个文件最后一行 → 第 i+1 个文件第一行
- [ ] `push()` 后前缀和增量更新，`total_bytes()` 正确
- [ ] 空 `FileSet` 不 panic（`total_lines() == 0`，`resolve` 恒为 `None`）

**Verification:**
- [ ] `cargo test --all -- fileset` 通过
- [ ] 单测用 `tests/fixtures/multi/` 三个文件构造 FileSet，断言全局行序列 == 三文件内容首尾相接
- [ ] proptest：随机 N 个随机行数的虚拟文件 → `resolve` 结果单调且覆盖完整
- [ ] 手工：打印 3 文件 FileSet 的 `total_lines` 与手工 `wc -l` 之和比对

**Dependencies:** T04
**Files:** `src/core/indexer.rs`, `tests/indexer_test.rs`
**Scope:** S (1–2 files)

---

> ### ✅ Checkpoint B：核心可信
> - [ ] 索引 7 类边界输入全过，属性测试通过
> - [ ] P1（1 GB < 2 s）、P3（≤8 B/行）达标
> - [ ] **人工确认**：多文件全局寻址正确

---

## Phase 2：能看见日志（垂直切片 S1）

### T06: 打开文件对话框与状态统计

**Description:** 顶部工具栏接 `rfd::FileDialog` 多选文件，调用 `LogFileIndex::open` 建立索引并装入 `FileSet`；底部状态栏显示文件数 / 总行数 / 总字节数。打通"磁盘 → 索引 → 应用状态"链路。

**Acceptance criteria:**
- [ ] "打开文件"支持多选，逐个建立索引；单个失败不影响其他文件，失败项记 `error!` 日志
- [ ] 加载时校验累计体积上限（单文件 16 GiB / 累计 32 GiB，见 `plan.md` Q9），超限拒绝并提示
- [ ] 状态栏显示：`已加载 N 个文件 / M 行 (X GB)`，字节数用人类可读单位（`util.rs` 格式化）
- [ ] 支持"关闭"（清空当前 FileSet，释放 mmap）
- [ ] 重复打开同一文件时给出明确行为（MVP：清空后重新加载）
- [ ] 加载大文件期间 UI 不冻结 → **若索引耗时明显，预留后续移入后台线程的接口**（MVP 允许同步，T19 实测后决定）

**Verification:**
- [ ] `cargo test --all -- util` 字节格式化单测通过
- [ ] 手工：打开 `tests/fixtures/multi/*` 三个文件，状态栏数字与 `wc -l` 总和一致
- [ ] 手工：打开 `empty.log`，状态栏行数 +0 且不 panic
- [ ] 手工：打开 1 GB 样本，观察加载期间窗口是否"未响应"（记录数据供 T19 决策）

**Dependencies:** T02, T05
**Files:** `src/app.rs`, `src/ui/toolbar.rs`, `src/ui/status_bar.rs`, `src/util.rs`
**Scope:** S (4 files)

---

### T07: 虚拟滚动日志区

**Description:** 中央区用 `ScrollArea::show_rows` 实现虚拟滚动，数据源接 `FileSet` 全局寻址。这是修复 D1（只能渲染第一个文件）的落地处，也是 P4/P5 性能验收的载体。

**Acceptance criteria:**
- [ ] `total_rows` 取 `fileset.total_lines()`，行内容经 `resolve()` 取，**不是 `files.first()`**
- [ ] 只用 `show_rows`，单帧渲染行数 = 视口行数 + 少量缓冲（应 ≤ 70 个 label）
- [ ] 等宽字体渲染，行高由 `TextStyle::Monospace` 决定
- [ ] 滚动到多文件交界处无空白行、无越界
- [ ] 空 FileSet 时显示引导文案而非空白/崩溃
- [ ] 滚动期间不做任何 O(总行数) 计算

**Verification:**
- [ ] 手工：打开 `tests/fixtures/multi/*`，从顶部滚到底部，逐屏检查无空白行
- [ ] 手工：1 GB 样本（1000 万行）滚动，egui FPS 叠加层 p95 帧耗时 < 16.6 ms（**P4**）
- [ ] 代码审查：确认渲染闭包内无 `clone()` 大对象、无正则编译、无文件 IO
- [ ] 手工：单帧 UI 节点数计数符合 **P5（≤ 视口行数 + 10）**

**Dependencies:** T06
**Files:** `src/ui/log_view.rs`, `src/app.rs`
**Scope:** M (2 files，但逻辑密集)

---

### T08: 行号列 + 折行 / 超长行截断

**Description:** 增加右对齐等宽行号列（宽度按最大行号位数自适应），提供折行开关，并对超长单行做渲染截断。截断不是可选项——单行 1 MB 会让 egui 字形布局从毫秒级劣化到秒级（`plan.md` A9 / R3）。

**Acceptance criteria:**
- [ ] 行号列右对齐、等宽、与正文基线对齐，宽度随最大行号位数自适应（1 万行 5 位、1 亿行 9 位）
- [ ] 行号从 1 开始（UI 展示）而非 0
- [ ] `Wrap` 开关：关闭时启用横向滚动，开启时文本折行
- [ ] 单行超过 **100,000 字符**时截断渲染并追加 `… (行过长已截断)` 提示（`plan.md` Q10）
- [ ] `tests/fixtures/long_line.log`（单行 1 MB）打开后窗口不卡死，滚动流畅

**Verification:**
- [ ] 手工：打开 `long_line.log`，确认 3 秒内可交互且显示截断提示
- [ ] 手工：切换 Wrap 开关，横向滚动与折行均正常
- [ ] 手工：1000 万行文件行号列宽度正确显示 8 位数字
- [ ] `cargo test --all` 通过（行号格式化若有纯函数则补单测）

**Dependencies:** T07
**Files:** `src/ui/log_view.rs`, `src/util.rs`
**Scope:** S (2 files)

---

> ### ✅ Checkpoint C：能看日志
> - [ ] 多文件连续滚动无空白行（D1 已修复）
> - [ ] 1 GB 样本 p95 帧耗时 < 16.6 ms（P4），单帧节点 ≤ 70（P5）
> - [ ] 单行 1 MB 不卡死
> - [ ] **端到端冒烟**：`tests/fixtures/multi/*` 三文件滚到底

---

## Phase 3：高亮

### T09: `highlight::segments` 纯函数与单测

**Description:** 实现 `spec.md` §7.6 的纯函数 `segments(line, highlighter) -> Vec<Segment>`。纯函数、不依赖 egui，便于单测。级别识别用**边界匹配**正则，修掉 PRD 里 `errors=3` 被误判为错误行的缺陷（D2）。

**Acceptance criteria:**
- [ ] `level_re` 匹配 `FATAL|ERROR|E|WARN|WARNING|W|INFO|I|DEBUG|D|TRACE|VERBOSE` 且要求词边界
- [ ] `errors=3`、`TERROR`、`INFORMATION` 之类干扰串**不匹配**为级别
- [ ] `Segment::{Plain, Level, Hit}` 三分段；`hit_re` 为 `None` 时不产生 `Hit`
- [ ] **不变式 4**：`segments()` 各分段文本拼接后与原始行逐字符相等
- [ ] 空行、纯空白行、超长行、含 U+FFFD 的行均正常返回
- [ ] 命中区间来自 `hit_re.find_iter()`，**不是** `str::match_indices`（G5）

**Verification:**
- [ ] `cargo test --all -- highlight` 通过，含 `levels.log` 干扰串用例
- [ ] proptest：随机行文本 + 随机 hit 正则 → 不变式 4 恒成立
- [ ] 单测：正则模式 `(?i)error.*timeout` 的命中区间与 `find_iter` 结果一致
- [ ] `cargo clippy` 无告警

**Dependencies:** T01（只需 regex 依赖，可与 T06–T08 并行）
**Files:** `src/highlight.rs`, `tests/highlight_test.rs`
**Scope:** S (2 files)

---

### T10: 高亮接入渲染

**Description:** 把 `segments()` 接到 `log_view` 的渲染回路上，按 `spec.md` §7.6 配色表输出彩色文本。同时落实 A8——高亮只对可视区执行，视口外不计算。

**Acceptance criteria:**
- [ ] 配色符合 §7.6 表：FATAL/ERROR `LIGHT_RED`、WARN `KHAKI`、INFO `LIGHT_GREEN`、DEBUG/TRACE `GRAY`、其它 `LIGHT_GRAY`
- [ ] 命中片段：前景 `BLACK` + 底色 `GOLD` + 粗体
- [ ] 分段渲染不引入额外横向间距（`item_spacing.x = 0`）
- [ ] 高亮计算**仅对 `row_range` 内的行执行**，视口外零计算（A8）
- [ ] `Highlighter` 实例在 app 中复用，`level_re` 与 `hit_re` **不逐行重建**
- [ ] 未检索时 `hit_re = None`，仅做级别着色

**Verification:**
- [ ] 手工：打开 `levels.log`，确认 ERROR/WARN/INFO/DEBUG 四色正确
- [ ] 手工：确认 `errors=3` 所在行**未**被染成红色（D2 已修复）
- [ ] 代码审查：确认渲染循环内无 `Regex::new`
- [ ] 手工：1 GB 文件滚动 p95 帧耗时仍 < 16.6 ms（高亮未拖慢帧率）

**Dependencies:** T08, T09
**Files:** `src/ui/log_view.rs`, `src/highlight.rs`, `src/app.rs`
**Scope:** S (3 files)

---

> ### ✅ Checkpoint D：能看颜色
> - [ ] `errors=3` 不着色（D2 已修复）
> - [ ] 高亮分段拼接无损（不变式 4）
> - [ ] 视口外零高亮计算（A8）

---

## Phase 4：检索

### T11: 检索契约类型定义

**Description:** 只落类型，不写实现：`SearchOptions` / `SearchMode` / `SearchHit` / `SearchMessage` / `SearchError` / `CancelToken`（`spec.md` §7.3–§7.5）。这是 T12–T14（引擎）与 T15（UI）并行的**前提契约**，双方都不得单方面修改。

**Acceptance criteria:**
- [ ] `SearchHit { file_idx: u32, line_idx: u32 }`，`Copy`，不存文本
- [ ] `SearchOptions { mode, case_sensitive, max_hits }`，`Default` = `(Plain, false, 2_000_000)`
- [ ] `SearchMessage` 五变体：`Partial` / `Truncated` / `Completed` / `Failed` / `Cancelled`
- [ ] `CancelToken` 基于 `Arc<AtomicBool>`，`Clone`，方法 `cancel()` / `is_cancelled()`
- [ ] `SearchError` 至少覆盖 `Regex(String)` / `Io(String)` / `Cancelled`
- [ ] 提供 `compile(options, pattern) -> Result<Regex, SearchError>`，四种模式映射符合 §7.5 表
- [ ] `core/search.rs` 不引用 egui

**Verification:**
- [ ] `cargo build --all` 通过
- [ ] `cargo test --all -- search::tests::compile` 通过：四种模式（Plain/Regex × 大小写）编译出的正则行为正确，含 `regex::escape` 生效用例
- [ ] 手工：`RUST_LOG=debug` 下无输出（本任务无运行时行为）

**Dependencies:** T05
**Files:** `src/core/search.rs`
**Scope:** XS (1 file)

---

### T12: 分块检索与进度回传

**Description:** 实现检索引擎主体：后台线程 + 按**字节**分块（目标 8 MiB，边界对齐到行）+ 每块推送 `Partial`。**先单线程跑通链路**，不上 rayon——先保证正确，T13 再加速。

**Acceptance criteria:**
- [ ] `std::thread::spawn` 起后台线程，主线程不阻塞
- [ ] 分块单位按字节（目标 8 MiB），**块边界对齐到行边界，不切断行**
- [ ] 每块完成后发送 `Partial { hits, bytes_done, bytes_total }`
- [ ] 全部完成发送 `Completed { hits, elapsed }`
- [ ] 正则编译失败 → `Failed(SearchError::Regex(msg))`，含 regex 原始错误文本
- [ ] 正则经 `RegexBuilder` 设置 `size_limit` / `dfa_size_limit`（R5）
- [ ] 发送端节流：距上次 send 不足 100 ms 则跳过本次 `Partial`（A11）
- [ ] 计数用块内本地累加 + 块末汇总，**不逐行 `fetch_add`**（A6/D7）

**Verification:**
- [ ] `cargo test --all -- search` 通过：小样本命中集合与朴素 `str::contains` 一致（不变式 3）
- [ ] 单测：`tests/fixtures/multi/` 跨文件检索，命中 `(file_idx, line_idx)` 坐标正确
- [ ] 手工：1 GB 样本检索 `ERROR`，观察结果**逐批出现**（非最后一次性出现）
- [ ] 手工：进度消息频率 ≤ 20 条/秒（**P9**）
- [ ] 手工：输入 `[` 触发正则错误，收到 `Failed` 且错误文本可读

**Dependencies:** T11
**Files:** `src/core/search.rs`, `tests/search_test.rs`
**Scope:** M (2 files)

---

### T13: rayon 并行化与计数优化

**Description:** 在块内引入 `rayon` 并行匹配，并落实 A6 的计数优化与 R11 的线程数限制。**只改性能，不改对外契约**——T12 的测试结果必须仍然全绿。

**Acceptance criteria:**
- [ ] 块内 `chunk.par_iter()` 并行匹配，块之间保持顺序推进（保证进度单调）
- [ ] 并行命中结果按全局顺序收集（结果顺序稳定，不因调度抖动）
- [ ] rayon 全局线程数限制为 `max(1, num_cpus - 1)`，给 UI 线程留核（R11）
- [ ] 块内计数本地累加，块末汇总（无逐行原子操作）
- [ ] `Mmap` 跨线程共享通过 `Arc`，无数据拷贝、无 lifetime 冲突

**Verification:**
- [ ] `cargo test --all` **全绿**（与 T12 结果一致，证明并行未破坏正确性）
- [ ] 手工：1 GB 样本正则检索吞吐 ≥ 200 MB/s（**P7**）
- [ ] 手工：1 GB 样本文本检索吞吐 ≥ 800 MB/s（**P8**）
- [ ] 手工：活动监视器确认多核被打满且 UI 仍流畅

**Dependencies:** T12
**Files:** `src/core/search.rs`, `src/main.rs`（rayon 线程池初始化）
**Scope:** S (2 files)

---

### T14: 取消 / 截断 / 失败路径

**Description:** 补齐检索引擎的三条控制流：取消（G1）、命中上限截断（G6）、各类失败路径。这让检索从"能跑"变成"可控"。

**Acceptance criteria:**
- [ ] 每个块开始前检查 `CancelToken`，已取消则发送 `Cancelled` 并**立即返回**
- [ ] 取消响应延迟 < 500 ms（**P10**），取消后不发送 `Completed`
- [ ] 命中数达到 `max_hits` 时停止扫描，发送 `Truncated`
- [ ] `Truncated` 后已收集的结果仍然可用（不是丢弃）
- [ ] 失败路径全覆盖：正则语法错误、文件读取错误，均发送 `Failed` 而非 panic
- [ ] 取消后线程确实退出（无残留线程占 CPU）

**Verification:**
- [ ] `cargo test --all -- search::tests::cancel` 通过：取消后线程在 500 ms 内退出
- [ ] 单测：`max_hits = 10` 检索 100 条命中的样本 → 收到 `Truncated` 且结果数为 10
- [ ] 手工：1 GB 样本检索中点"停止"，CPU 回落到空闲
- [ ] 手工：不变量 5（取消正确性）成立

**Dependencies:** T13
**Files:** `src/core/search.rs`, `tests/search_test.rs`
**Scope:** M (2 files)

---

### T15: 检索 UI

**Description:** 顶部工具栏加检索输入框、模式下拉、大小写开关、搜索/停止按钮；底部加进度条与内联错误提示；中央区在"结果模式"下渲染命中行。依赖 T11 的契约，可与 T12–T14 并行开发。

**Acceptance criteria:**
- [ ] 输入框支持回车触发；检索中按钮变为"停止"
- [ ] 模式下拉：`纯文本` / `正则`，默认纯文本
- [ ] 大小写开关 `Aa`，默认关闭（忽略大小写）
- [ ] 进度条显示真实百分比 `bytes_done / bytes_total`，带 `show_percentage()`
- [ ] 正则语法错误在输入框下方**内联红字**展示 regex 原文错误，不弹模态框、不崩溃
- [ ] 结果模式下渲染命中行，命中片段用 GOLD 底色高亮（复用 T09/T10）
- [ ] 检索中**不禁用**滚动区与停止按钮；仅禁用"打开文件"/"搜索"/"导出"
- [ ] 每帧 drain 上限 1000 条消息（A11）
- [ ] 已有检索时启动新检索，先 `cancel()` 旧任务

**Verification:**
- [ ] 手工：四种模式组合检索，命中集合符合预期
- [ ] 手工：进度条从 0% 平滑增长到 100%，无跳变（D5 已修复）
- [ ] 手工：输入 `[` → 内联红字提示，程序照常运行
- [ ] 手工：检索期间滚动日志区依然流畅，最长卡顿帧 < 50 ms（**P6**）
- [ ] 手工：命中超过上限时状态栏显示"结果已截断"

**Dependencies:** T11（契约）；渲染部分依赖 T10
**Files:** `src/ui/toolbar.rs`, `src/ui/status_bar.rs`, `src/ui/log_view.rs`, `src/app.rs`
**Scope:** M (4 files)

---

> ### ✅ Checkpoint E：能检索
> - [ ] 边搜边出结果，进度条平滑
> - [ ] 取消响应 < 500 ms（P10）
> - [ ] 正则错误内联提示不崩溃
> - [ ] P6/P7/P8/P9 达标
> - [ ] **人工确认**：四种模式语义正确

---

## Phase 5：导出

### T16: `core::export` 流式导出

**Description:** 实现 `spec.md` §7.8 的流式导出：按 `SearchHit` 坐标逐行取 `line_bytes()` 直接写入 `BufWriter`，不做 UTF-8 转换，保证与源文件逐字节一致。

**Acceptance criteria:**
- [ ] 通过 `Arc<FileSet>` + `Arc<Vec<SearchHit>>` 共享数据，**不 clone 大 Vec**
- [ ] 逐行取 `line_bytes()` 写入 8 MiB `BufWriter`，全程无 UTF-8 往返转换
- [ ] `ExportFormat::RawLines` 写原始行；`WithPrefix` 写 `<文件名>:<行号>:` 前缀（用文件名**不是** file_index）
- [ ] 每 10,000 行或每 200 ms 推送一次 `Progress`
- [ ] 支持 `CancelToken`，取消后退出并保留已写部分（提示用户）
- [ ] 目标路径不可写时发送 `Failed`，不 panic
- [ ] 导出 100 万行峰值内存增量 < 100 MB（**P11**）

**Verification:**
- [ ] `cargo test --all -- export` 通过
- [ ] 单测：导出文件内容与源文件命中行**逐字节相同**（不变式 6），含含无效 UTF-8 的样本
- [ ] 单测：`WithPrefix` 格式前缀为文件名而非序号
- [ ] 手工：导出 1 GB 样本的 100 万行结果，计时 < 10 s（**P11**）
- [ ] 手工：导出中途取消，文件保留已写部分且不损坏

**Dependencies:** T14
**Files:** `src/core/export.rs`, `src/core/mod.rs`, `tests/search_test.rs`（或新增 `export_test.rs`）
**Scope:** M (3 files)

---

### T17: 导出 UI

**Description:** 工具栏加"导出结果"按钮：`rfd` 保存对话框 → 格式勾选 → 后台导出 → 底部进度反馈。

**Acceptance criteria:**
- [ ] "导出结果"在无命中时禁用
- [ ] 保存对话框默认文件名带时间戳（如 `hyper-log-export-20260901-231100.log`）
- [ ] 格式勾选：默认 `RawLines`（`plan.md` Q5 待确认）
- [ ] 导出中按钮显示"导出中…"并可取消
- [ ] 底部状态栏显示导出进度与完成路径
- [ ] 导出失败时内联显示错误原因

**Verification:**
- [ ] 手工：检索后导出，用文本编辑器打开确认内容正确
- [ ] 手工：`diff` 校验导出文件前 10 行与源文件对应行一致
- [ ] 手工：导出中取消，状态栏正确恢复
- [ ] 手工：导出到不可写路径（如 `/`），显示错误且不崩溃

**Dependencies:** T16
**Files:** `src/ui/toolbar.rs`, `src/ui/status_bar.rs`, `src/app.rs`
**Scope:** S (3 files)

---

> ### ✅ Checkpoint F：能导出
> - [ ] 导出命中行与源文件逐字节一致
> - [ ] P11 达标（100 万行 < 10 s，内存增量 < 100 MB）
> - [ ] 导出可取消

---

## Phase 6：打磨

### T18: 空闲重绘策略与 drain 上限

**Description:** 落实 `spec.md` §8.4：无后台任务时不 `request_repaint`，避免 egui 持续满帧重绘烧 CPU；消息处理加每帧上限。

**Acceptance criteria:**
- [ ] 仅当 `is_searching || is_exporting` 时请求重绘
- [ ] 空闲 30 秒后 CPU 占用 < 2%（**P12**）
- [ ] 每帧 `try_recv` drain 上限 1000 条，超出部分留到下一帧
- [ ] 窗口需要重绘时（如缩放、鼠标悬停）egui 自身仍能正常触发重绘
- [ ] 后台任务结束后最后一帧正确清理进度状态

**Verification:**
- [ ] 手工：空闲状态观察活动监视器 30 秒，CPU < 2%（**P12**）
- [ ] 手工：检索期间 FPS 正常，结束后 CPU 立刻回落
- [ ] 代码审查：确认 `request_repaint` 的唯一调用点在后台活动分支内

**Dependencies:** T15, T17
**Files:** `src/app.rs`
**Scope:** S (1 file)

---

### T19: 大文件性能实测与调优

**Description:** 用 `scripts/gen_log.sh` 生成的 1 GB / 10 GB 样本，逐项实测 `spec.md` §11.2 的 12 项性能指标，不达标的当场调优。

**Acceptance criteria:**
- [ ] P1：1 GB 打开 < 2.0 s
- [ ] P2：10 GB 打开 < 20 s
- [ ] P3：索引内存 ≤ 8 B/行
- [ ] P4：1000 万行滚动 p95 帧耗时 < 16.6 ms
- [ ] P5：单帧 UI 节点 ≤ 视口行数 + 10
- [ ] P6：检索期间最长卡顿帧 < 50 ms
- [ ] P7：正则检索 ≥ 200 MB/s
- [ ] P8：文本检索 ≥ 800 MB/s
- [ ] P9：进度消息 ≤ 20 条/秒
- [ ] P10：取消响应 < 500 ms
- [ ] P11：导出 100 万行 < 10 s，内存增量 < 100 MB
- [ ] P12：空闲 CPU < 2%
- [ ] 全部实测值填入 `spec.md` §11.2 表格（新增"实测值"列）
- [ ] 不达标项：要么调优达标，要么在 `spec.md` 记录偏差与原因（**不允许静默放过**）

**Verification:**
- [ ] 逐项跑 `cargo run --release` + 打点计时，数据填入表格
- [ ] 若 P3 不达标：对 <4 GiB 文件改用 `Vec<u32>` 存偏移（4 B/行）
- [ ] 若 P1/P2 不达标：索引构建并行化（`rayon` 分块扫描换行符）
- [ ] 若 T06 记录的加载冻结明显：把 `open()` 移入后台线程 + 进度反馈
- [ ] Windows 平台复测关键指标 P1/P4/P7

**Dependencies:** T18
**Files:** `src/core/indexer.rs`, `src/core/search.rs`, `src/app.rs`, `tests/perf_test.rs`, `spec.md` §11.2
**Scope:** M (性能敏感，跨多文件)

---

### T20: 冒烟清单执行与 README

**Description:** 执行 `spec.md` §11.3 的 8 项手工冒烟清单，补齐 `README.md`，并把实际依赖版本、实测性能数据、遗留问题回写 `spec.md` §14 修订记录。

**Acceptance criteria:**
- [ ] §11.3 八项冒烟全部执行并记录结果（通过 / 不通过 + 说明）
- [ ] `README.md` 含：项目简介、构建与运行命令（release 为准）、快捷键说明、已知限制（Windows mmap 锁文件、文件被外部 truncate、不支持 32 位）
- [ ] `spec.md` §4 依赖版本为**实际解析版本**
- [ ] `spec.md` §14 修订记录补充本轮实施结论
- [ ] `spec.md` §13 开放问题中的已决项标记为"已确认"
- [ ] 全量门禁通过：`fmt --check` + `clippy -D warnings` + `test --all`

**Verification:**
- [ ] 八项冒烟清单逐项勾选
- [ ] `cargo fmt --all -- --check` 退出码 0
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` 退出码 0
- [ ] `cargo test --all` 全绿
- [ ] 换一台干净机器按 README 从零构建成功

**Dependencies:** T19
**Files:** `README.md`, `spec.md`, `docs/plan.md`
**Scope:** S (文档为主)

---

> ### ✅ Checkpoint G：完工
> - [ ] P12 达标，§11.2 全部 12 项达标或已记录偏差
> - [ ] §11.3 八项冒烟全过
> - [ ] `spec.md` §14 修订记录已更新
> - [ ] 三份文档（`spec.md` / `plan.md` / `tasks.md`）状态一致

---

## 附录 A：任务总表

| ID | 任务 | 阶段 | 规模 | 依赖 | 可并行 |
| --- | --- | --- | --- | --- | --- |
| T01 | 依赖与构建配置 | 0 | XS | — | — |
| T02 | 应用骨架与窗口启动 | 0 | S | T01 | ∥ T03 |
| T03 | 测试脚手架与 CI | 0 | M | T01 | ∥ T02 |
| T04 | `LogFileIndex` 内存映射与行索引 | 1 | M | T03 | — |
| T05 | `FileSet` 全局行寻址 | 1 | S | T04 | — |
| T06 | 打开文件对话框与状态统计 | 2 | S | T02, T05 | ∥ T09 |
| T07 | 虚拟滚动日志区 | 2 | M | T06 | ∥ T09 |
| T08 | 行号列 + 折行/超长行截断 | 2 | S | T07 | ∥ T09 |
| T09 | `highlight::segments` 纯函数与单测 | 3 | S | T01 | ∥ T06–T08 |
| T10 | 高亮接入渲染 | 3 | S | T08, T09 | — |
| T11 | 检索契约类型定义 | 4 | XS | T05 | — |
| T12 | 分块检索与进度回传 | 4 | M | T11 | ∥ T15 |
| T13 | rayon 并行化与计数优化 | 4 | S | T12 | ∥ T15 |
| T14 | 取消 / 截断 / 失败路径 | 4 | M | T13 | ∥ T15 |
| T15 | 检索 UI | 4 | M | T11 | ∥ T12–T14 |
| T16 | `core::export` 流式导出 | 5 | M | T14 | — |
| T17 | 导出 UI | 5 | S | T16 | — |
| T18 | 空闲重绘策略与 drain 上限 | 6 | S | T15, T17 | — |
| T19 | 大文件性能实测与调优 | 6 | M | T18 | ∥ T20(部分) |
| T20 | 冒烟清单执行与 README | 6 | S | T19 | — |

## 附录 B：与 spec.md 里程碑的映射

| spec.md 里程碑 | 本文件任务 |
| --- | --- |
| M0 骨架 | T01, T02, T03 |
| M1 indexer | T04, T05 |
| M2 UI / 虚拟滚动 | T06, T07, T08 |
| M3 高亮 | T09, T10 |
| M4 search | T11, T12, T13, T14, T15 |
| M5 export | T16, T17 |
| M6 打磨 | T18, T19, T20 |
