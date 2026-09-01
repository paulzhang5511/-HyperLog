# Spec: Hyper Log —— 高性能桌面日志查看器

| 项目 | 内容 |
| --- | --- |
| 需求来源 | `docs/prd.md`（Hyper Log 系统设计文档） |
| 文档状态 | **待评审**（Phase 1: SPECIFY） |
| 目标读者 | 实现者（人 / Agent） |
| 创建日期 | 2026-09-01 |
| 工具链基线 | rustc / cargo 1.95.0，edition 2024 |

> **用法说明**：本文件是"写代码之前"的唯一事实来源。任何实现决策与本文冲突时，**先改本文再改代码**。
> 本文覆盖了 PRD 未定义或存在缺陷的部分（见 §1.2 / §1.3），这些补充即本次 SPECIFY 的主要产出。

---

## 1. 需求分析（对 `docs/prd.md` 的评估）

### 1.1 PRD 已明确、可直接采纳的部分

| # | 需求 | PRD 位置 | 结论 |
| --- | --- | --- | --- |
| R1 | `memmap2` 零拷贝映射 + 行偏移索引 | §4.1 | 采纳（需修正空文件 / UTF-8 / CRLF，见 §1.3） |
| R2 | `ScrollArea::show_rows` 虚拟滚动 | §4.3 | 采纳（需修复多文件寻址，见 D1） |
| R3 | 后台线程 + `rayon` 并行正则检索 | §4.2 | 采纳（需补取消与流式结果，见 D3/D4） |
| R4 | `crossbeam-channel` 非阻塞回传 | §2 | 采纳（需补背压与每帧 drain 上限，见 D6） |
| R5 | 进度通信协议 `SearchProgress` | §5-1 | 采纳（需在字节维度分块，见 D5） |
| R6 | 分块并发检索 + 进度条 UI | §5-2 / §5-3 | 采纳（需改为边算边吐结果，见 D4） |
| R7 | 检索结果流式导出到新文件 | §4.2 | 采纳（需改导出格式与内存模型，见 D8） |
| R8 | 日志级别 + 关键字高亮 | §4.3 | 采纳（需与正则检索语义对齐，见 D2） |
| R9 | 桌面端（macOS / Windows） | §1 | 采纳 |

### 1.2 需求缺口（PRD 未定义，本 Spec 补齐）

按严重度排序。**P0 = MVP 不成立，P1 = 体验缺陷，P2 = 增强。**

| ID | 缺口 | 严重度 | 本 Spec 的裁决 |
| --- | --- | --- | --- |
| G1 | 无**取消**机制：10 GB 文件检索一旦启动无法中止，UI 全局禁用 | P0 | §7.4 `CancelToken`（`Arc<AtomicBool>`），取消响应 < 500 ms |
| G2 | 多文件**加载后只能看第一个文件**（PRD 代码 `self.files.first()`） | P0 | §7.2 引入全局行寻址，修复后再谈多文件 |
| G3 | 无效 UTF-8 行被**静默丢弃**（`from_utf8().ok()` → `None`） | P0 | §7.1 `get_line` 返回 `Cow<str>`，lossy 转换 |
| G4 | 无检索模式选择（只有正则） | P1 | §7.5 三种模式：纯文本 / 正则，大小写敏感开关，**默认纯文本+忽略大小写** |
| G5 | 高亮用字面量子串，检索用正则 → **命中与高亮不一致** | P1 | §7.6 高亮复用同一编译后的 `Regex`，按 `find_iter` 区间着色 |
| G6 | 无结果集上限：1 亿条命中会 OOM | P1 | §7.5 默认上限 2,000,000 条，超限停止并提示 |
| G7 | 超长行被截断，无法横向滚动 / 折行 | P1 | §7.7 提供 `Wrap` 开关（默认关，横向滚动） |
| G8 | 导出用 `file_index`（0,1,2…）而非文件名，且格式硬编码 | P1 | §7.8 导出**原始行**，前缀可选，前缀用文件名 |
| G9 | 无空文件 / 单行无换行 / BOM / CRLF 处理约定 | P1 | §7.1 明确边界规则，配单测 |
| G10 | 无错误展示规范（仅 `eprintln!`） | P1 | §8 用 `log` + 状态栏内联错误条 |
| G11 | 无打包 / 分发约定 | P2 | §13 开放问题（MVP 用 `cargo build --release`） |
| G12 | 无 Follow / tail 实时追加 | P2 | **明确排除在本 Spec 范围外** |

### 1.3 PRD 设计缺陷（代码级 Review）

| ID | 问题 | 位置 | 修正方案 |
| --- | --- | --- | --- |
| D1 | `total_rows` 汇总所有文件行数，渲染却只读 `files.first()` → 越界行渲染为空白 | PRD §4.3 L509-533 | 全局行寻址（§7.2） |
| D2 | 高亮按 `text.contains("ERROR")` 全行匹配 → 含 "errors=" 的 URL 误判 | PRD §4.3 L332 | 边界匹配（§7.6） |
| D3 | 检索线程无法中断，且 `is_searching` 期间"打开文件"按钮被禁用 | PRD §4.3 L429 | `CancelToken`（§7.4） |
| D4 | 结果在**全部完成后**一次性回传 → 大文件首条命中延迟 = 全量耗时 | PRD §5-2 | 每块回传 `Partial`（§7.4） |
| D5 | 按固定 **行** 数（50,000）分块 → 行长差异导致块耗时抖动，进度条不均匀 | PRD §5-2 L620 | 按**字节**分块（目标 8 MiB/块）（§7.4） |
| D6 | 无背压：块完成即 `send`，小块会打满 channel；`try_recv` 无 drain 上限 | PRD §5-2 | 节流 + 每帧 drain 上限（§7.4 / §8.4） |
| D7 | 每行一次 `AtomicUsize::fetch_add` → 伪共享，核数越多越亏 | PRD §5-2 L642 | 块内本地累加，块末一次性汇总（§7.4） |
| D8 | `SearchResult.match_content: String` 全量复制 + `search_results.clone()` 导出 → 内存随命中数线性膨胀 | PRD §4.2 L209 | 结果只存 `(file_idx, line_idx)`，内容惰性解析（§7.3） |
| D9 | `unsafe { Mmap::map(&file) }` 无 SAFETY 注释；**长度为 0 的文件会 panic** | PRD §4.1 L112 | 长度校验 + SAFETY 注释 + 空文件返回 0 行（§7.1 / §8.3） |
| D10 | `line_offsets = vec![0]` 使空文件被当作 1 行 | PRD §4.1 L115 | 空文件 → `line_offsets = vec![]`（§7.1） |
| D11 | PRD 称索引内存"~1% 文件大小"，实际为 **8 B/行**（100 B/行 ≈ 8%） | PRD §2-1 | 修正为 8 B/行，并写入验收标准 §11 |
| D12 | 未限制正则的 DFA 大小 → 病态正则可吃满内存 | PRD §4.2 | `RegexBuilder` 设 size/dfa 上限（§7.5） |
| D13 | PRD 目录结构仅 3 个文件，加入进度/取消/高亮后 `main.rs` 会膨胀到 800+ 行 | PRD §3.1 | 拆分为 6 个模块（§6），**待确认** |
| D14 | PRD 写 `edition = "2021"`，仓库 `Cargo.toml` 为 `edition = "2024"` | PRD §3.2 | 以仓库为准：`edition = "2024"` |
| D15 | mmap 在 Windows 上会锁定文件（其他进程无法删除/覆写）；macOS 上日志被 truncate 会 SIGBUS | §4.1 | MVP 接受，写入已知限制（§13）；提供只读提示 |

---

## 2. Objective

### 2.1 一句话目标

为**开发 / 运维 / 测试**人员提供一款桌面端日志查看器，能在**秒级打开 GB 级文本日志**、**千万行级别流畅滚动**、并在**不冻结界面**的前提下完成跨文件正则检索与结果导出。

### 2.2 用户故事（MVP）

| ID | 作为… | 我希望… | 以便… |
| --- | --- | --- | --- |
| US1 | 开发者 | 拖入/选择一个或多个 1~10 GB 的 `.log` 文件后 2 秒内可滚动浏览 | 不用再等 `less`/记事本加载 |
| US2 | 开发者 | 在 1000 万行文件中滚动、拖拽窗口时保持 60 FPS | 定位问题时不被 UI 卡顿干扰 |
| US3 | 开发者 | 输入关键字/正则后**边搜边出结果**，并有真实百分比进度 | 知道还要等多久，不用盲等 |
| US4 | 开发者 | 检索中随时点"停止"立刻中止 | 输错正则或范围不对时能马上重来 |
| US5 | 开发者 | ERROR/WARN/INFO/DEBUG 按级别着色，命中词带底色 | 一眼扫出异常行 |
| US6 | 测试工程师 | 把筛选结果导出为新文件（原始行，可选文件名:行号前缀） | 贴到缺陷单里给开发复现 |
| US7 | 开发者 | 检索语法错误时界面内联提示，而不是程序静默无反应 | 自己修正表达式 |

### 2.3 非目标（本 Spec 明确不做）

- 实时 Follow / tail 文件追加（日志被别的进程持续写入）
- 压缩文件（`.gz` / `.zip`）直接读取
- 结构化日志解析（JSON / logfmt 字段提取与列视图）
- 多标签页 / 分屏对比 / 书签与批注
- 远程日志（SSH / HTTP / 云存储）
- 时间点过滤（按时间戳范围筛选）

---

## 3. 假设清单（ASSUMPTIONS）

> 以下是我基于 PRD 推断出的假设。**如有错误请现在纠正**，否则我将按此实现。

1. **目标平台**：macOS（Apple Silicon 优先）与 Windows 11，桌面端原生窗口，非 Web / 移动端。
2. **输入格式**：纯文本日志，UTF-8 为主，允许混杂无效字节（按 lossy 处理）；不涉及二进制日志。
3. **文件可变性**：MVP 视日志文件为**只读**——加载后不追踪外部修改；如被 truncate，行为未定义（见 §13）。
4. **单文件上限**：MVP 支持最大 16 GB / 单文件；超过则提示并拒绝加载（避免 mmap 与索引内存的极端情况）。
5. **文件数量上限**：单次加载 ≤ 32 个文件。
6. **检索默认语义**：默认**纯文本、忽略大小写**；切换到正则模式后大小写开关仍然生效（`(?i)` 由开关统一控制）。
7. **结果上限**：默认 2,000,000 条，命中超限即停止并在状态栏提示"（已截断）"。
8. **导出编码**：与源文件一致（写原始字节），UTF-8 输出，不写 BOM。
9. **依赖版本**：以 PRD §3.2 指定版本为基线（`egui`/`eframe` 0.36.1 等）；实现时若 `cargo add` 解析出更高兼容版本，需在此表更新并记录理由。
10. **代码组织**：允许偏离 PRD §3.1 的三文件结构，按 §6 拆分模块（对应 D13）。
11. **日志与可观测性**：引入 `log` + `env_logger`，替代 PRD 中的裸 `println!`/`eprintln!`。
12. **分发**：MVP 不打包安装包，交付 `cargo build --release` 产物。

---

## 4. Tech Stack

| 层次 | 选型 | 版本 | 用途 |
| --- | --- | --- | --- |
| 语言 | Rust | edition **2024**（rustc 1.95） | — |
| GUI | `egui` + `eframe` | 0.36.1（`wgpu` feature） | 即时模式 UI，虚拟滚动 |
| 文件映射 | `memmap2` | 0.9 | 零拷贝读取 |
| 并行计算 | `rayon` | 1.10 | 多核正则匹配 |
| 正则 | `regex` | 1.10 | 检索与高亮（共用编译结果） |
| 跨线程通信 | `crossbeam-channel` | 0.5.16 | 后台 → UI 消息 |
| 文件对话框 | `rfd` | 0.15 | 打开 / 保存对话框 |
| 日志 | `log` + `env_logger` | 0.4 / 0.11 | 替代 `println!` |
| 时间格式化 | `chrono` | 0.4.45 | 导出默认文件名本地时间戳（T17，M5 新增） |
| 序列化（配置） | *待定* | — | 仅在需要持久化窗口/偏好时引入（P2） |

**发布构建 profile**（追加到 `Cargo.toml`）：

```toml
[profile.release]
opt-level = 3
lto = "thin"        # 链接期优化，显著提升正则/mmap 热路径
codegen-units = 1
panic = "abort"     # GUI 应用不需要 unwind
strip = true
```

> **注意**：`dev` profile 下的正则匹配慢 10~50 倍。所有性能验收（§11）必须在 `--release` 下测量。

### 4.1 egui / eframe 0.36 实际 API（实测确认，与 PRD 示例不同）

PRD §4.3 的示例代码基于旧版 egui API。在 **egui / eframe 0.36.1** 上实测，以下四点必须按新 API 实现：

| 项 | PRD 示例（旧） | 0.36.1 实际 | 影响 |
| --- | --- | --- | --- |
| App 入口 | `fn update(&mut self, ctx: &Context, frame: &Frame)` | 拆成 `fn logic(&mut self, ctx: &Context, frame: &mut Frame)` + `fn ui(&mut self, ui: &mut Ui, frame: &mut Frame)` | 消息轮询放 `logic`（窗口隐藏时也会被调，后台任务照样推进）；绘制放 `ui` |
| 面板 | `egui::TopBottomPanel::top(id).show(ctx, …)` | `egui::Panel::top(id).show(ui, …)`；`Panel::bottom/left/right` | `TopBottomPanel` 已不存在；`show` 收 `&mut Ui` 而非 `&Context` |
| 中央区 | `egui::CentralPanel::default().show(ctx, …)` | `egui::CentralPanel::default().show(ui, …)` | 同上；且 **CentralPanel 必须最后添加** |
| egui 引用 | `use eframe::egui;` | `use egui;`（`egui` 为直接依赖） | eframe 0.36 不再重导出 egui |

`ScrollArea::show_rows(ui, row_height_sans_spacing, total_rows, FnOnce(&mut Ui, Range<usize>))` 与 `ViewportBuilder::with_inner_size / with_min_inner_size` 与 PRD 一致，未变。

---

## 5. Commands

```bash
# 开发运行（调试构建，不含性能优化，仅用于功能验证）
cargo run

# 性能与验收必须使用 release
cargo run --release

# 构建
cargo build --release

# 测试
cargo test --all                       # 单元 + 集成
cargo test --all -- --ignored          # 含大文件性能用例（CI 不跑）
cargo test --all -- --nocapture        # 查看测试输出

# 覆盖率（需 cargo-llvm-cov）
cargo llvm-cov --all --html --open

# 静态检查
cargo fmt --all -- --check             # 格式检查（不写盘）
cargo fmt --all                        # 自动格式化
cargo clippy --all-targets --all-features -- -D warnings   # 门禁：0 warning

# 依赖维护
cargo add <crate>                      # 加依赖后必须回到 §4 更新版本表
cargo outdated                         # 需 cargo-outdated
cargo deny check                       # 需 cargo-deny：许可证 / 安全审计

# 生成测试用大文件（10 GB 约 1000 万行）
scripts/gen_log.sh <目标路径> <行数>
```

---

## 6. Project Structure

```
hyper-log/
├── Cargo.toml
├── docs/
│   ├── prd.md                 # 需求来源（只读，不修改）
│   └── spec.md                # ← 本文件（唯一事实来源）
├── scripts/
│   └── gen_log.sh             # 生成 GB 级测试日志
├── src/
│   ├── main.rs                # 仅：窗口配置 + eframe 启动 + 日志初始化
│   ├── app.rs                 # LogViewerApp：状态机、update()、消息轮询
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── toolbar.rs         # 顶部：打开文件 / 检索框 / 模式 / 停止 / 导出
│   │   ├── status_bar.rs      # 底部：状态文本 + 进度条 + spinner
│   │   └── log_view.rs        # 中央：虚拟滚动日志区
│   ├── highlight.rs           # 级别配色 + 命中区间着色（纯函数，易单测）
│   ├── core/
│   │   ├── mod.rs
│   │   ├── indexer.rs         # LogFileIndex / FileSet：mmap + 行偏移（PRD log_indexer.rs）
│   │   ├── search.rs          # SearchEngine + 消息协议（PRD search_engine.rs）
│   │   └── export.rs          # 流式导出
│   └── util.rs                # 字节格式化、行数估算等
├── tests/
│   ├── fixtures/              # 小样本日志（含 CRLF / BOM / 无效 UTF-8 / 空文件）
│   ├── indexer_test.rs
│   ├── search_test.rs
│   ├── highlight_test.rs
│   └── perf_test.rs           # #[ignore]，需 1 GB+ 样本
└── benches/
    └── index_bench.rs         # criterion（M4 引入）
```

**规则**：
- `src/core/**` **禁止**出现 `egui` 引用 —— 保证核心逻辑可无头测试（对应 §9 测试策略）。
- `src/ui/**` 只做渲染，不持有业务状态。
- `src/main.rs` 保持 < 60 行。

---

## 7. 架构与模块契约

### 7.1 `core::indexer` —— 内存映射与行索引

```rust
use std::sync::Arc;
use memmap2::Mmap;

/// 单个已索引的日志文件
pub struct LogFileIndex {
    pub path: PathBuf,
    mmap: Arc<Mmap>,              // 私有：禁止外部持有裸切片跨越生命周期
    line_offsets: Vec<usize>,     // 每行起始字节偏移，长度 == 总行数
}

/// 多文件集合：把 "文件 i 的第 j 行" 统一映射为 0..total_lines 的全局行号
pub struct FileSet {
    files: Vec<Arc<LogFileIndex>>,
    /// cumulative[i] = 前 i 个文件的行数之和，长度 = files.len() + 1
    cumulative: Vec<usize>,
}

impl LogFileIndex {
    /// 打开并建立索引。已处理：空文件、BOM、CRLF、无尾换行。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexError>;

    /// 总行数（空文件为 0，见 D10）
    pub fn line_count(&self) -> usize;

    /// 取第 line_idx 行（0-based）。
    /// - 返回 Cow::Owned 当且仅当该行含无效 UTF-8（lossy 替换，见 G3）
    /// - 行尾的 `\r`（CRLF）与 `\n` 已被剥离
    pub fn line(&self, line_idx: usize) -> Option<Cow<'_, str>>;

    /// 原始字节切片（导出用，避免 UTF-8 往返损失）
    pub fn line_bytes(&self, line_idx: usize) -> Option<&[u8]>;

    /// 索引占用的堆内存（字节），用于状态栏显示与 §11 验收
    pub fn index_memory_bytes(&self) -> usize { self.line_offsets.len() * 8 }
}

impl FileSet {
    pub fn push(&mut self, file: Arc<LogFileIndex>);
    pub fn total_lines(&self) -> usize;
    /// 全局行号 → (file_idx, line_idx)；越界返回 None
    pub fn resolve(&self, global_idx: usize) -> Option<(usize, usize)>;
    pub fn line(&self, global_idx: usize) -> Option<Cow<'_, str>>;
    pub fn total_bytes(&self) -> usize;
}
```

**边界规则（必须单测覆盖，对应 G9/D10）**：

| 输入 | 期望 `line_count()` | 说明 |
| --- | --- | --- |
| 空文件（0 字节） | 0 | mmap 长度 0 时会 panic，需提前短路（D9） |
| `"abc"`（无尾换行） | 1 | 最后一行不以 `\n` 结尾仍算一行 |
| `"a\nb\n"` | 2 | 尾随 `\n` 不产生第 3 行 |
| `"\n\n"` | 2 | 两个空行 |
| `"a\r\nb\r\n"` | 2，内容 `"a"` / `"b"` | 剥离尾部 `\r` |
| UTF-8 BOM + `"x\n"` | 1，内容 `"x"` | 跳过首部 BOM |
| 含 `\xFF` 无效字节 | 行数不变，内容含 U+FFFD | lossy（G3） |

### 7.2 全局行寻址（修复 D1）

渲染不再 `files.first()`，而是：

```rust
// src/ui/log_view.rs
let total_rows = if in_result_mode { state.results.len() } else { fileset.total_lines() };
ScrollArea::vertical().show_rows(ui, row_height, total_rows, |ui, range| {
    for row in range {
        let line: Cow<str> = if in_result_mode {
            let (f, l) = results[row];            // 只存坐标，见 D8
            fileset.file(f).line(l).unwrap_or_default()
        } else {
            fileset.line(row).unwrap_or_default()
        };
        highlight::render_line(ui, &line, &highlighter);
    }
});
```

### 7.3 `core::search` —— 数据模型（修复 D8）

```rust
/// 命中项：只存坐标，不复制文本
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub file_idx: u32,
    pub line_idx: u32,      // 0-based
}
```

**理由**：1 GB 日志若有 500 万行命中，存 `String` 需约 500 MB；存坐标只需 40 MB，且渲染时可惰性解析。导出时再逐行取 `line_bytes` 流式写出（§7.8）。

### 7.4 `core::search` —— 检索引擎与消息协议

```rust
/// 检索模式
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode { #[default] Plain, Regex }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    pub mode: SearchMode,
    pub case_sensitive: bool,   // 默认 false
    pub max_hits: usize,        // 默认 2_000_000
}

/// 后台 → UI
pub enum SearchMessage {
    /// 增量结果：每完成一个字节块推送一次（修复 D4）
    Partial { hits: Vec<SearchHit>, bytes_done: u64, bytes_total: u64 },
    /// 命中已达上限而提前终止（G6）
    Truncated { hits: usize },
    /// 正常完成
    Completed { hits: usize, elapsed: Duration },
    /// 失败：正则语法错误 / IO 错误 / 被取消
    Failed(SearchError),
    /// 已响应取消并退出
    Cancelled,
}

/// 取消令牌（填补 G1）
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn cancel(&self) { self.0.store(true, Ordering::Relaxed); }
    pub fn is_cancelled(&self) -> bool { self.0.load(Ordering::Relaxed) }
}
```

**执行模型（修复 D5 / D7 / D6）**：

```
spawn(后台线程)
  ├─ 编译 pattern（Plain 模式转义为字面量；case_sensitive=false 时前置 (?i)）
  ├─ 按【字节】切块：目标 8 MiB/块，块边界对齐到行（不切断行）
  └─ for chunk in chunks:
        ├─ 检查 cancel → 发送 Cancelled 并 return
        ├─ chunk.par_iter() 并行匹配（块内本地累加，块末一次性汇总 → 修复 D7）
        ├─ 限流：距上次 send 不足 100 ms 则跳过本次 Partial（修复 D6）
        └─ 发送 Partial { hits, bytes_done, bytes_total }
     发送 Completed
```

**关键约束**：
- 分块单位必须是**字节**而非行数（D5）。
- 每块内的原子计数用**本地累加 + 块末汇总**，禁止逐行 `fetch_add`（D7）。
- 正则统一通过 `RegexBuilder::new(&p).size_limit(16 MiB).dfa_size_limit(8 MiB)` 构造（D12）。
- Plain 模式用 `regex::escape(pattern)` 编译，从而**高亮与检索复用同一 `Regex`**（G5）。
- 新检索启动前必须先 `cancel()` 旧任务，或禁止并发（MVP 选择：禁止并发 + 提供停止按钮）。

### 7.5 检索语义（填补 G4 / G6）

| 模式 | 大小写 | 实际编译的表达式 |
| --- | --- | --- |
| Plain | 忽略（默认） | `(?i)regex::escape(input)` |
| Plain | 敏感 | `regex::escape(input)` |
| Regex | 忽略 | `(?i)input` |
| Regex | 敏感 | `input` |

- `max_hits` 达到后停止扫描，发送 `Truncated` 并在状态栏显示"结果已截断（>200 万条），请缩小范围"。
- 正则编译失败 → `SearchMessage::Failed(SearchError::Regex(msg))`，UI 在输入框下方用红色文本展示 **regex 的错误原文**，不弹模态框。

### 7.6 `highlight` —— 着色规则（修复 D2 / G5）

```rust
pub struct Highlighter {
    level_re: Regex,   // \b(FATAL|ERROR|E|WARN|WARNING|W|INFO|I|DEBUG|D|TRACE|VERBOSE)\b
    hit_re: Option<Regex>,  // 与检索同一个 Regex，仅在检索时 Some
}

pub enum Segment<'a> { Plain(&'a str), Level(&'a str, Level), Hit(&'a str) }

/// 纯函数：输入一行 + 高亮器 → 分段列表。可脱离 egui 单测。
pub fn segments<'a>(line: &'a str, h: &Highlighter) -> Vec<Segment<'a>>;
```

配色（**默认主题**，后续可配置）：

| 级别 | 颜色（egui `Color32`） | 说明 |
| --- | --- | --- |
| FATAL / ERROR | `Color32::LIGHT_RED` | 边界匹配，`errors=3` 不误判 |
| WARN / WARNING | `Color32::KHAKI` | |
| INFO | `Color32::LIGHT_GREEN` | |
| DEBUG / TRACE / VERBOSE | `Color32::GRAY` | |
| 其它 | `Color32::LIGHT_GRAY` | |
| 命中片段 | 前景 `Color32::BLACK` + 底色 `Color32::GOLD` + 粗体 | |

> 命中判定必须用 `hit_re.find_iter(line)` 的**区间**，禁止 `str::match_indices`（G5）。

### 7.7 UI 布局

```
┌──────────────────────────────────────────────────────────────┐
│ [打开文件] [关闭] │ 检索: [____________] [模式▾][Aa] [搜索/停止] │
│                    │ [导出结果] [折行]                          │
├──────────────────────────────────────────────────────────────┤
│ 1  2026-09-01 10:00:01.123  INFO  ... (虚拟滚动)              │
│ 2  2026-09-01 10:00:01.456  ERROR ...                        │
│ ...                                                          │
├──────────────────────────────────────────────────────────────┤
│ 已加载 3 个文件 / 12,345,678 行 (1.2 GB) │ [======>   ] 62%   │
└──────────────────────────────────────────────────────────────┘
```

- 行号列：右对齐等宽，宽度按最大行号位数自适应。
- `Wrap` 关闭时启用 `ScrollArea` 双向滚动；开启时 `show_rows` 的行高估算需改为按字符数动态计算（P1，MVP 可先固定）。
- 检索中**不禁用**滚动区与"停止"按钮（G1）；仅禁用"打开文件""导出""搜索"自身。

### 7.8 `core::export` —— 流式导出（修复 D8 / G8）

```rust
pub enum ExportFormat { RawLines, WithPrefix }   // 前缀 = "<文件名>:<行号>:"
pub enum ExportMessage { Progress { done: usize, total: usize },
                         Completed { path: PathBuf, bytes: u64 },
                         Failed(String), Cancelled }

pub fn export_async(
    files: Arc<FileSet>,
    hits: Arc<Vec<SearchHit>>,     // Arc 共享，禁止 clone 大 Vec
    dest: PathBuf,
    format: ExportFormat,
    cancel: CancelToken,
    tx: Sender<ExportMessage>,
);
```

- 逐行取 `line_bytes()` 直接写入 `BufWriter`（8 MiB 缓冲），**不做 UTF-8 转换**，保证与源文件字节一致。
- 每 10,000 行或每 200 ms 推送一次 `Progress`。
- 导出前检查目标路径可写；写入失败发送 `Failed` 并保留部分文件的现实（不做清理，提示用户）。

---

## 8. Code Style

### 8.1 通用约定

- 格式化：`rustfmt` 默认配置（`cargo fmt`，**不做自定义 rustfmt.toml**）。
- Lint：`clippy -D warnings`，禁止在核心模块使用 `#![allow(...)]` 之外的全局抑制。
- 命名：类型 `UpperCamelCase`，函数/变量 `snake_case`，常量 `SCREAMING_SNAKE_CASE`。
- 公开 API 必须有 `///` 文档注释；`unsafe` 块必须有 `// SAFETY:` 注释（对应 D9）。
- 错误处理：**库代码返回 `thiserror` 风格的具体错误类型**（MVP 可手写一个 `enum Error`），禁止在 `core/` 里 `unwrap()` / `expect()`；UI 层把错误降级为用户可见文本。
- 禁止裸 `println!`/`eprintln!`，统一 `log::{info, warn, error}`。

### 8.2 参考实现（风格样例）

以"打开并索引文件"为例，体现上述全部约定：

```rust
// src/core/indexer.rs
use std::{borrow::Cow, fs::File, path::Path, sync::Arc};
use memmap2::Mmap;

/// 建立索引时可能发生的错误。
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("file is empty: {0}")]
    Empty(std::path::PathBuf),
    #[error("file too large: {bytes} bytes (limit {limit})")]
    TooLarge { bytes: u64, limit: u64 },
    #[error("io error on {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// 单次加载允许的最大文件体积：16 GiB。
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// UTF-8 BOM。
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

impl LogFileIndex {
    /// 打开 `path` 并建立行偏移索引。
    ///
    /// 索引不改变文件内容：行尾的 `\n` 与 `\r\n` 会在 [`Self::line`] 读取时剥离。
    /// 空文件返回 [`IndexError::Empty`]，因为 `Mmap::map` 对 0 长度文件会 panic。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| IndexError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let len = file.metadata().map_err(|source| IndexError::Io {
            path: path.to_path_buf(),
            source,
        })?.len();

        if len == 0 {
            return Err(IndexError::Empty(path.to_path_buf()));
        }
        if len > MAX_FILE_BYTES {
            return Err(IndexError::TooLarge { bytes: len, limit: MAX_FILE_BYTES });
        }

        // SAFETY: `file` 以只读方式打开且在本函数之后不再被写入；
        // 调用方不得通过其它句柄 truncate 该文件（见 spec §13 已知限制）。
        // `len > 0` 已在上文保证，避免 memmap2 对空映射 panic。
        let mmap = unsafe { Mmap::map(&file) }.map_err(|source| IndexError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let body = if mmap.starts_with(BOM) { &mmap[BOM.len()..] } else { &mmap[..] };
        let mut line_offsets = Vec::with_capacity(len as usize / 80); // 预估平均行长 80 B
        if !body.is_empty() {
            line_offsets.push(0);
        }
        for (i, &b) in body.iter().enumerate() {
            if b == b'\n' && i + 1 < body.len() {
                line_offsets.push(i + 1);
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            mmap: Arc::new(mmap),
            line_offsets,
        })
    }

    /// 取第 `line_idx` 行（0-based），越界返回 `None`。
    ///
    /// 行内容不包含行尾的 `\n` / `\r\n`。当该行包含无效 UTF-8 时返回
    /// `Cow::Owned`，其中无效字节被替换为 U+FFFD（不丢行）。
    pub fn line(&self, line_idx: usize) -> Option<Cow<'_, str>> {
        let bytes = self.line_bytes(line_idx)?;
        Some(String::from_utf8_lossy(bytes))
    }
}
```

### 8.3 `unsafe` 使用规则

本项目仅允许在以下位置出现 `unsafe`：**`Mmap::map` 调用点**（`core/indexer.rs`）。任何新增的 `unsafe` 都必须在 PR 中说明理由并获得评审。

### 8.4 UI 层每帧约束（对应 D6）

```rust
// src/app.rs —— 每帧最多处理 1000 条消息，防止后台洪水饿死渲染
const MAX_MSG_PER_FRAME: usize = 1000;

// 注意：这是 eframe 0.36 的 App::logic 回调，不是 App::ui —— 见 §4.1。
// logic 不绘制任何 UI，且在窗口隐藏时也会被调用，因此后台任务可继续推进。
fn poll_backend(&mut self, ctx: &egui::Context) {
    let mut n = 0;
    while n < MAX_MSG_PER_FRAME {
        match self.search_rx.try_recv() {
            Ok(msg) => { self.handle_search_msg(msg); n += 1; }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    // 仅后台有活动时才请求重绘，空闲时不烧 CPU
    if self.is_searching || self.is_exporting {
        ctx.request_repaint();
    }
}
```

---

## 9. Testing Strategy

### 9.1 分层

| 层次 | 范围 | 工具 | 位置 | 目标 |
| --- | --- | --- | --- | --- |
| 单元 | 纯函数、数据结构 | `#[cfg(test)]` 模块内 | `src/**`（就近） | 索引/高亮/分块/转义逻辑 |
| 集成 | 跨模块行为 | `#[test]` | `tests/*.rs` | 打开→检索→导出全链路 |
| 属性 | 随机输入不变式 | `proptest` | `tests/` | 索引 round-trip、行覆盖完整性 |
| 性能 | 大文件 | `#[ignore]` + 手工跑 | `tests/perf_test.rs` | §11 全部指标 |

### 9.2 必须覆盖的核心不变式

1. **行覆盖完整性**：对任意文件，`Σ line(i).len() + 行数 == 有效字节数`（`\r` 已被剥离时按规则调整）。随机二进制 + 随机换行序列。
2. **索引单调性**：`line_offsets` 严格递增且全部 `< mmap.len()`。
3. **检索等价性**：对同一文件，`Plain` 模式命中集合 == 朴素 `str::contains` 命中集合（大小写语义一致时）。
4. **高亮无损**：`segments()` 拼接结果 == 原始行文本（逐字符相等）。
5. **取消正确性**：`cancel()` 后不产生 `Completed`，且线程在 500 ms 内退出。
6. **导出保真**：导出文件中命中行的字节序列与源文件对应行**逐字节相同**。

### 9.3 Fixtures（`tests/fixtures/`，纳入版本控制）

| 文件 | 内容 | 覆盖 |
| --- | --- | --- |
| `empty.log` | 0 字节 | D9 / D10 |
| `no_trailing_newline.log` | 3 行，末行无 `\n` | G9 |
| `crlf.log` | CRLF 换行 | G9 |
| `bom.log` | UTF-8 BOM + 2 行 | G9 |
| `invalid_utf8.log` | 混入 `\xFF\xFE` | G3 |
| `levels.log` | ERROR/WARN/INFO/DEBUG 各若干，含 `errors=3` 干扰串 | D2 |
| `long_line.log` | 单行 1 MB | G7 |
| `multi/` | 3 个文件 | D1 多文件寻址 |

### 9.4 大文件样本

`scripts/gen_log.sh <path> <lines>` 生成合成日志（时间戳 + 级别 + 随机正文，平均行长 ~100 B）。
**不入库**，需要时本地生成：

```bash
scripts/gen_log.sh /tmp/bench_1gb.log 10_000_000   # ≈ 1 GB
```

### 9.5 CI（GitHub Actions，M1 建立）

```yaml
- cargo fmt --all -- --check
- cargo clippy --all-targets --all-features -- -D warnings
- cargo test --all
- matrix: [macos-latest, windows-latest] × [stable]
```

覆盖率目标：`core/` ≥ 80%；`ui/` 不设硬性目标（GUI 难以自动化，靠手工冒烟清单 §11.3）。

---

## 10. Boundaries

### Always（无需询问，每次都做）

- 提交前跑 `cargo fmt` + `cargo clippy -D warnings` + `cargo test --all`。
- 新增/升级依赖后，同步更新本文档 §4 版本表。
- 公开函数写 `///` 文档注释；`unsafe` 写 `// SAFETY:`。
- 所有用户可见字符串走 `log` 或 UI 状态文本，禁止裸 `println!`。
- 修改需求或架构决策时，**先改 `docs/spec.md` 再改代码**。
- 性能相关的改动必须在 `--release` 下实测前后数据。

### Ask First（需明确确认）

- 增加或升级第三方依赖（尤其是 `unsafe` 密集 / 许可证非 MIT-Apache 的 crate）。
- 修改 `core/` 的公开 API 签名（影响 §7 契约）。
- 改动 CI 配置、release profile、构建脚本。
- 引入持久化（配置文件、最近打开文件列表）。
- 变更 §11 中的任何验收指标。
- 引入新的 `unsafe` 代码。

### Never（禁止）

- 在 `core/` 中引用 `egui` / `eframe`（破坏可测试性）。
- 在 `core/` 中使用 `unwrap()` / `expect()` / `panic!()`。
- 忽略或删除失败/跳过的测试来让 CI 变绿。
- 提交密钥、绝对路径硬编码、个人环境配置。
- 未加 `// SAFETY:` 注释的 `unsafe` 块。
- 擅自扩大范围，实现 §2.3 非目标中的功能（Follow / 压缩 / 结构化解析…）。
- 在 UI 线程执行任何 O(文件行数) 的同步操作。

---

## 11. Success Criteria

### 11.1 功能验收（可勾选）

- [ ] 选择 1~3 个日志文件后，状态栏显示正确的文件数 / 总行数 / 总字节数。
- [ ] 多文件场景下可连续滚动浏览**全部**文件内容，无空白行、无越界（D1）。
- [ ] 输入关键字回车 → 结果逐批出现，进度条从 0% 平滑增长到 100%（D4/D5）。
- [ ] 检索过程中点击"停止"，< 500 ms 内状态恢复空闲且 CPU 回落（G1）。
- [ ] 检索中主窗口可正常滚动、缩放、拖动，无"未响应"（R4）。
- [ ] 正则语法错误 → 输入框下方红字显示错误原文，程序不崩溃（G4）。
- [ ] ERROR/WARN/INFO/DEBUG 按 §7.6 配色；`errors=3` 之类干扰串不着色（D2）。
- [ ] 命中片段底色与检索语义一致（正则模式下能高亮正则命中区间）（G5）。
- [ ] 导出文件可用任意文本编辑器打开，命中行与源文件**逐字节一致**（D8/G8）。
- [ ] 空文件 / CRLF / BOM / 无效 UTF-8 / 单行 1 MB 样本全部正常，不 panic（G9）。
- [ ] 命中超过 200 万条时停止并提示截断（G6）。

### 11.2 性能验收（**必须在 `--release` 下测量**）

测试机基线：Apple M 系列或 8 核 x86，16 GB RAM。样本：`gen_log.sh` 生成，平均行长 ~100 B。

| # | 指标 | 目标 | 测量方式 |
| --- | --- | --- | --- |
| P1 | 打开 1 GB（≈1000 万行）到可滚动 | **< 2.0 s** | 打点 `LogFileIndex::open` 耗时 |
| P2 | 打开 10 GB（≈1 亿行）到可滚动 | **< 20 s** | 同上 |
| P3 | 索引常驻内存 | **≤ 8 B/行**（1 亿行 ≈ 800 MB） | `index_memory_bytes()`（修正 D11） |
| P4 | 滚动 1000 万行文件帧率 | **p95 帧耗时 < 16.6 ms**（60 FPS） | egui 内置 FPS 叠加层（`ctx.set_debug_on_hover` 或 `--cfg` 统计） |
| P5 | 单帧渲染 UI 节点数 | **恒定 ≤ 视口行数 + 10**（约 60） | 手工计数 + 代码审查 |
| P6 | 检索期间 UI 最长卡顿帧 | **< 50 ms** | 帧耗时直方图 p99 |
| P7 | 正则检索吞吐（8 核） | **≥ 200 MB/s** | `bytes_total / elapsed` |
| P8 | 纯文本（字面量）检索吞吐 | **≥ 800 MB/s** | 同上 |
| P9 | 进度消息频率 | **≤ 20 条/秒**，且间隔 ≥ 100 ms | 计数 + 节流断言（D6） |
| P10 | 取消响应延迟 | **< 500 ms** | 打点 `cancel()` 到线程退出 |
| P11 | 导出 100 万行结果 | **< 10 s**，峰值内存增量 **< 100 MB** | 计时 + `/usr/bin/time -l` 或任务管理器 |
| P12 | 空闲时（无后台任务）CPU 占用 | **< 2%** | 活动监视器观察 30 s（§8.4 重绘策略） |

### 11.3 手工冒烟清单（每个里程碑执行）

1. 冷启动 → 窗口正常，无控制台错误。
2. 打开 `tests/fixtures/multi/*` 三个文件 → 滚动到底无空白。
3. 打开 1 GB 样本 → 计时、观察内存。
4. 检索 `ERROR` → 观察进度条与增量结果。
5. 检索中途点击停止 → 观察 CPU 回落。
6. 输入 `[` 触发正则错误 → 观察内联提示。
7. 导出结果为 `WithPrefix` → 用 `diff` 校验前 10 行与源文件一致。
8. 反复开关文件 10 次 → 无内存持续增长（简易泄漏检查）。

---

## 12. 实施阶段

> **详细方案与任务清单已拆出，以那两份文档为准：**
> - 技术方案、依赖图、风险、检查点 → `docs/plan.md`
> - 20 个任务的验收标准与验证步骤 → `docs/tasks.md`

| 里程碑 | 内容 | 依赖 | 对应任务 | 出口条件 |
| --- | --- | --- | --- | --- |
| **M0** | 骨架：`Cargo.toml` 依赖、`main.rs` 窗口、CI、`scripts/gen_log.sh` | — | T01–T03 | 空白 egui 窗口可启动；CI 全绿 |
| **M1** | `core::indexer` 完整实现 + `FileSet` 全局寻址 + 单元测试 | M0 | T04–T05 | §7.1 边界规则全过；P1/P2/P3 达标 |
| **M2** | 浏览：打开文件 + 虚拟滚动 + 行号列 + 折行/截断 | M1 | T06–T08 | §11.1 前 2 项；P4/P5 达标 |
| **M3** | 高亮：`segments` 纯函数 + 渲染接入 | M2 | T09–T10 | §11.1 级别着色项；不变式 4 |
| **M4** | 检索：契约 + 分块并行 + 进度 + 取消 + UI | M3 | T11–T15 | §11.1 第 3~6 项；P6~P10 达标 |
| **M5** | 导出：流式写出 + UI | M4 | T16–T17 | §11.1 第 7~11 项；P11 达标 |
| **M6** | 打磨：重绘策略、性能实测调优、冒烟与 README | M5 | T18–T20 | P12 达标；冒烟清单全过 |

每个任务遵循：≤ 5 个文件、带验收标准与验证命令（命令清单见 §5，任务清单见 `docs/tasks.md`）。

### 12.1 实施进度（截至 2026-09-02）

| 里程碑 | 状态 | 提交 |
| --- | --- | --- |
| **M0** | ✅ 完成 | `d2af691` / `e30d827` / `ff585c0` |
| **M1** | ✅ 完成 | `1f99542`（含 T04–T05） |
| **M2** | ✅ 完成 | `1f99542`（含 T06–T08） |
| **M3** | ✅ 完成 | `fbb1b64`（T09–T10） |
| **M4** | ✅ 完成 | `b3ccc15`（T11–T15） |
| **M5** | ⏳ 待开始 | — |
| **M6** | ⏳ 待开始 | — |

> 说明：M1/M2 合并于同一提交，因为 `core::indexer` 的 API 必须被 UI 消费后才不会触发
> `clippy -D warnings` 的 dead_code 门禁，单独提交索引器会使 CI 在合并前变红。

---

## 13. Open Questions（需人工决策）

| # | 问题 | 我的默认建议 | 影响 |
| --- | --- | --- | --- |
| Q1 | 是否接受偏离 PRD §3.1 的三文件结构，改为 §6 的模块化拆分？ | **接受**（PRD 结构撑不下进度/取消/高亮） | 目录结构、D13 |
| Q2 | 目标平台是否包含 Linux？ | MVP **不含**（`rfd`/`wgpu` 需额外验证） | CI matrix |
| Q3 | 是否需要"最近打开的文件"持久化？ | MVP **不做**（需要引入配置文件依赖，见 §10 Ask First） | 功能范围 |
| Q4 | 结果上限 2,000,000 是否合适？ | 保持默认，M5 后按实测调 | G6 |
| Q5 | 导出默认格式用 `RawLines` 还是 `WithPrefix`？ | **默认 `RawLines`**（可直接被其它工具消费），UI 提供勾选 | G8 |
| Q6 | 是否需要处理"日志文件在打开期间被其它进程 truncate / 删除"？ | MVP **不处理**，在状态栏提示"文件已被修改，请重新加载"需文件监听（P2） | D15 |
| Q7 | 配色是否需要浅色/深色主题自适应？ | MVP 用 egui 默认主题，§7.6 配色为**默认主题**下的取值，主题切换留到 M5 之后 | §7.6 |
| Q8 | 单文件上限 16 GB 是否合适？32 位平台不支持 | 保持 16 GB；**明确不支持 32 位平台** | §3 假设 4 |
| Q9 | 多文件累计加载体积上限？（规划阶段新增，防内存叠加） | 单文件 16 GiB，**累计 32 GiB**，超限拒绝并提示 | §3 假设 5 / T06 |
| Q10 | 单行渲染截断阈值？（规划阶段新增，防 egui 字形布局卡死） | **100,000 字符**，超出显示 `… (行过长已截断)` | T08 / G7 |

---

## 14. 修订记录

| 日期 | 变更 | 作者 |
| --- | --- | --- |
| 2026-09-01 | 基于 `docs/prd.md` 创建初版：完成需求差距分析（15 项缺口/缺陷）、补齐模块契约、量化验收标准 | Agent |
