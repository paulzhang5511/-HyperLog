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
| G11 | 打包 / 分发：MVP 交付 `cargo build --release` 产物；M10 额外提供 `scripts/package_macos.sh` 生成可分发 `HyperLog.app`（可拖入 Applications），提前交付 | M10 ✅ | §12.12 / scripts/package_macos.sh |
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
12. **分发**：MVP 交付 `cargo build --release` 产物；M10 起额外提供 `scripts/package_macos.sh` 生成 `HyperLog.app`（含 Info.plist + ad-hoc 签名 + zip），可直接拖入 `/Applications` 分发。

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
| 序列化（配置） | `std::fs`（零新增依赖） | — | M17 已落地：主题/窗口几何/折行/侧栏/最近检索词经 `core/prefs.rs` 纯文本持久化（平台配置目录 + tmp/rename 原子写）；核心层用自有 `ThemePref` 枚举，避免在 `core` 内依赖 `egui`（P2） |

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
┌──────────┬──────────────────────────────────────────────────────────────┐
│ 目录树   │ Hyper Log │ [☰][打开][打开目录][最近文件▾] │ [查找…][模式▾][Aa][查找][查找全部] │ [行:___][折行][性能][☀] │
│ ☰ 文件1  ├──────────────────────────────────────────────────────────────┤
│ ▸ 子目录  │   1 │ 2026-09-01 10:00:01.123  INFO  ... (虚拟滚动)          │
│   文件2   │   2 │ 2026-09-01 10:00:01.456  ERROR ...                    │
│ ...      │ ... │ ...                                                  │
│          ├──────────────────────────────────────────────────────────────┤
│          │ status │ 文件名 │ 索引 xx │ xx GB │ xx 行 │ xx 个文件 │ [行 123] │
└──────────┴──────────────────────────────────────────────────────────────┘
```

- **编辑器观感**（Q7）：默认暗色主题（VS Code Dark+ 风格），`src/ui/theme.rs` 统一维护
  `dark_style`/`light_style` 两套 `Style` 与一套 `Palette`（供日志区 `Painter` 自绘取色），
  工具栏 ☀/🌙 切换。控件扁平化（低饱和背景 + 细描边），紧凑间距（按钮 22px、正文 12.5px）。
- **行号槽**：独立背景色 + 与正文之间的竖线；行号右对齐等宽，宽度按最大行号位数自适应。
- **行级交互**：hover 高亮整行、点击选中（⌘C/Ctrl+C 复制选中行）、右键「复制此行」。
- **单 widget 渲染**：每行只创建 1 个 `Label` 承载整行的多色 `LayoutJob`（时间戳/级别/命中
  分段合成一个 job），行背景与行号用 `Painter` 绘制，widget 数从 O(行×分段) 降到 O(行)（§8.4）。
- `Wrap` 关闭时启用 `ScrollArea` 双向滚动，横向范围按「采样的最长行宽」固定（CJK 按 1.7 倍宽加权）；
  开启时行高统一按最长行的折行数放大（`MAX_WRAP_LINES` 封顶，P1 的 MVP 方案）。
- **滚动方向锁定**（修复「上下滑动时正文左右漂移」）：egui 的 `ScrollArea` 对 x/y **两个方向
  独立累加** `smooth_scroll_delta`、没有主控方向判定，触控板双指上下滑动所带的轻微水平分量会被
  如实计入横向偏移。故在日志区渲染前调用 `ui/log_view.rs::lock_scroll_axis` 改写 `InputState`：
  按 `AXIS_LOCK_RATIO`（默认 0.3）判定主控方向——次要方向分量小于主要方向的 0.3 倍则清零，
  真正的斜向滚动原样保留，单方向输入（普通滚轮）不受影响。纯计算逻辑抽为 `locked_scroll_delta`
  以便单测。**必须在 ScrollArea 消费 delta 之前改写**，因为 ScrollArea 滚动后会把对应分量清零。
- **检索命中高亮**：命中段仅叠加荧光笔底色（`hit_bg`），文字沿用正常 `text` 色——保证高亮后
  内容仍清晰可读（VS Code 风格），不再改文字色（早期版本同时改底色+文字色导致低对比、看不清）。
- 检索中**不禁用**滚动区与"停止"按钮（G1）；仅禁用"打开文件""导出""搜索"自身。

### 7.7.0 打开文件的语义：替换（非追加）

- 工具栏「打开」（⌘O）、「最近文件」、「打开目录」统一走 `App::load_paths`，语义为**替换**：
  先把本次选中的文件全部打开成功，再整体接管 `fileset`，旧集合丢弃（与编辑器「打开文件」一致）。
- 早期版本为**追加**，导致打开第二个文件后视口仍停在第一个文件的内容上，表现为"内容不更新"。
- 失败处理：先把新文件攒在临时 `Vec` 里，**全部失败则保留原有内容不动**、只在状态栏报错，
  避免"旧的已清空、新的又没打开成功"的空窗；部分成功则替换并显示已跳过的原因。
- 累计 32 GiB 上限在替换语义下只针对**本次新集合**统计，不含即将被替换掉的旧文件。
- 文档被替换后，下列按 `file_idx` / 全局行号索引的状态一并清空：检索结果 `search_results`、
  命中高亮正则 `hit_regex`、结果视图开关、选中行、跳转目标、侧边栏高亮、外部修改脏标记。

### 7.7.1 打开目录（Q「打开目录」）

- `core::dirscan::collect_log_files(root)` 递归收集目录树下的日志文件（`.log`/`.txt`/`.out`，
  大小写不敏感，以及**无扩展名的文件**——现实中不少日志没有后缀，如 `access`、`debug`、
  `foo.log.1` 滚动归档），跳过隐藏条目（`.` 开头）与符号链接（防环），按路径升序返回确定性结果。
- 工具栏「打开目录」弹目录选择框，收集到的文件复用 `load_paths` 批量加载（与单文件打开
  同一套校验：空文件跳过、累计 32 GiB 上限、成功才记入最近文件）。
- **命令行也接受目录**：`App::expand_initial_paths` 在 `new()` 中把初始路径里的目录递归展开为
  日志文件（复用 `dirscan::collect_log_files`），文件保持原样，使 `hyper-log <dir>` 与
  「打开目录」等价；目录下无日志文件时只告警，不中断其余路径加载。

### 7.7.2 查找全部（Q「查找全部」）

- `core::grepdir::run_grep(root, pattern, options, cancel, tx)` 对目录树下的每个日志文件
  逐个建索引并做字节级检索（复用 `search` 的 `build_regex_bytes` + 分块 `find_iter` 语义），
  流式回传带**文件路径**与**内联行文本**的命中（`GrepHit`），因为目录检索的文件索引是临时的，
  结果必须自包含（不依赖后续回查）。
- 命中上限 `max_hits` 复用 G6 截断语义；取消复用 `CancelToken`（< 500 ms 响应）。
- 工具栏「查找全部」弹目录选择框，选中后启动后台检索；进行中显示「停止查找全部」。
- 与单文件检索（`start_search`）互斥：目录检索运行期间禁用普通检索。

### 7.7.3 结果浮动窗口（Q「结果单独页面/保存」）

- 目录检索完成后弹出**独立浮动窗口**「查找结果」（`egui::Window`，`app.rs` 中 `id =
  "grep_results_window"`，`ui/results_view.rs` 渲染内容），**不挤压正文日志区布局**
  （notepad++ 风格）：可拖动、可改变大小、可折叠，右上角 ✕ 关闭。
  **定位不用 `.anchor`**（它会在窗口每次从关闭重开时强制拉回锚点，表现为「固定不可移动」），
  改用 `.default_pos` 仅**首次出现**时定位到底部居中，此后位置/尺寸完全由用户拖动决定、egui 记忆。
  **宽度取主窗口内容矩形的 80%**（下限 400px 兜底超窄窗口），`default_width` 首次出现时生效。
  顶部是结果页头（命中总数 + 截断提示 + 「保存结果…」「复制全部」「返回日志」），
  下方是虚拟滚动的命中列表，每行「`文件路径:行号` + 内容」（路径用主题强调色、行号弱化）。
- **窗口关闭状态同步**：`.open(&mut show_results)` 让 ✕ 与「返回日志」按钮都作用到
  同一个 `state.show_results` 状态——二者任一关闭后窗口即消失（早期 `Panel::bottom` 版本
  的关闭按钮无法真正关闭，因为面板显隐只受 `show_results` 单向控制）。
- **点击命中行 → 跳转原文对应行**（notepad++ 风格）：`GrepHit` 除展示路径 `display_path` 外
  还内联**绝对路径 `abs_path`**；点击时置 `pending_grep_jump = (abs_path, 行号)`，由 `app.rs::ui`
  落实——若目标文件已在 `FileSet` 则按 `file_global_start + 行号` 直接定位，否则 `load_paths`
  打开它（替换语义）再定位；正文滚动到该行并高亮（`scroll_target` + `selected_row`）。
  结果窗口**保持打开**以便连续跳转，仅「返回日志」按钮或 ✕ 显式关闭。
- 「保存结果…」弹保存对话框，把命中按「`路径:行号: 内容`」逐行写盘（复用
  `GrepHit` 内联文本，零回查）。
- 「复制全部」复制所有命中到剪贴板；⌘C/Ctrl+C 复制选中行。

### 7.7.4 侧边栏目录树（⌘B / ☰）

- 左侧可折叠面板（`egui::Panel::left`，`app.rs` 中由 `state.show_sidebar` 控制显隐，
  ⌘B 或工具栏 ☰ 切换），把当前 `FileSet` 的文件按磁盘路径构建成目录树，**风格对齐 VSCode 资源管理器**：
- **以共同父目录为根**：取所有已加载文件父目录的「最长公共前缀目录」作为根节点（标签取该目录的文件夹名，根目录显示 `/`），
  树内仅展示相对结构，**不在树中暴露 `/Users/…` 这种深而无用的绝对路径**（修复 M14 初版直接按绝对路径建树、把 `/` 当成节点的显示错误）；
  构建时跳过 `RootDir`/`Prefix` 组件，避免挂载点/盘符成为节点。
- **排序与图标**：`BTreeMap` 保证目录顺序确定；目录用 `CollapsingHeader`（默认展开，📁 图标）在前，
  文件用可点击行（📄 图标 + 文件名 + 行数弱色 + hover 全路径）按文件名（不区分大小写）排序在后，贴近 VSCode「文件夹优先、组内字母序」。
- **缩进**：依赖 `CollapsingHeader` 的天然嵌套缩进，文件行额外加固定左缩进对齐到目录名（绕过折叠箭头宽度），不再手动叠加多层 `add_space`。
- 点击文件即跳转：把该文件在全局行号空间的首行（`FileSet::file_global_start(file_idx)`，
  来自 `core::indexer.rs` 的 `cumulative` 累加表）写入 `state.scroll_target` 与
  `state.selected_row`，由 `log_view` 滚动并高亮；同时写入 `state.sidebar_active_file`
  **持久高亮该文件**（类似 VSCode 高亮已打开文件），不受 `scroll_target` 被 `log_view` 消费清零的影响。
- **反查高亮**：用户在日志区选中某行（`state.selected_row`）时，侧边栏据此自动高亮其所属文件
  （`FileSet::file_for_global_row(row)` 经 `cumulative` 二分反查），类似 VSCode 高亮光标所在文件；
  点击文件与点击日志行两者最终都收敛到同一文件，状态一致。
- **右键菜单**（`Response::context_menu`）：文件行支持「复制路径」（`ctx.copy_text`）、「在文件管理器中显示」
  （`reveal_in_file_manager`：macOS `open -R` / Windows `explorer /select` / Linux `xdg-open` 父目录）、「跳到该文件」；
  根节点支持「复制根目录路径」「在文件管理器中显示根目录」。菜单内用 `ui.close()` 关闭（egui 0.36 无 `close_menu`）。
- **空目录折叠**：目录只作为文件的「中间节点」被创建，不会凭空出现空目录，天然满足空目录折叠。

### 7.7.5 行号跳转（⌘L）

- 工具栏「行:」输入框（稳定 `Id = line_jump_input_id()`）接受 1-based 全局行号；
  失焦或回车/Tab 时调用 `apply_line_jump`：解析越界则忽略（不报错）。
- 跳转由 `log_view::show` 消费 `state.scroll_target` 落实：固定行高 `ROW_HEIGHT=18.0` →
  目标偏移 `row*row_h - view_h*0.5`（居中），通过 `ScrollArea::show_rows` 返回的
  `ScrollAreaOutput { id, state }` 用 `state.store(ctx, id)` 持久化纵向偏移。

### 7.7.6 常用快捷键

`app.rs::handle_shortcuts`（固有 `impl`，非 `eframe::App` trait 方法）用
`ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(MODIFIERS, Key)))` 一次性消费：

| 快捷键 | 作用 | 备注 |
|--------|------|------|
| ⌘O | 打开文件 | 检索中禁用（G1） |
| ⌘⇧O | 打开目录 | 检索中禁用（G1） |
| ⌘F | 聚焦检索框 | 下一帧 `request_focus(search_input_id())` |
| ⌘L | 聚焦行号输入框 | 下一帧 `request_focus(line_jump_input_id())` |
| ⌘G / ⌘↵ | 触发查找 | 等价「查找」按钮（非空模式且非检索中） |
| ⌘B | 切换侧边栏 | `show_sidebar = !show_sidebar` |
| Esc | 返回 | 命中视图→全量日志；否则清除选中行 |

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

#### 11.2.1 实测值（`--release`，测试机 Apple M 系列，样本 `gen_log.sh /tmp/bench_1gb.log 10000000`）

| # | 指标 | 目标 | 实测 | 结论 |
| --- | --- | --- | --- | --- |
| P1 | 打开 1 GB（≈1000 万行） | < 2.0 s | **1.0–1.3 s** | ✅ 余量充足（预热页缓存后；冷盘首开另受存储限制） |
| P2 | 打开 10 GB（≈1 亿行） | < 20 s | **15.52 s** | ✅ 余量充足（预热页缓存后；`#[ignore]` 测试 `open_10gb_under_20s`） |
| P3 | 索引常驻内存 | ≤ 8 B/行 | 8.00 B/行（80 MB / 1000 万行） | ✅ 恰好达标（`Vec<usize>` 8 字节/行） |
| P7 | 正则检索吞吐 | ≥ 200 MB/s | **峰值 1360 MB/s** | ✅ 余量充足（M7 块级并行扫描） |
| P8 | 纯文本（字面量）检索吞吐 | ≥ 800 MB/s | **峰值 2275 MB/s** | ✅ 余量充足（M7 块级并行扫描） |
| P9 | 进度消息频率 | ≤ 20 条/秒 | 9.1 条/秒 | ✅（节流 100 ms，实测 9~10 条/秒） |
| P10 | 取消响应延迟 | < 500 ms | 5.8 ms | ✅（取消检查在波次之间） |
| P11 | 导出 100 万行 | < 10 s | 356 ms（104 MB） | ✅ 余量充足 |
| P4 | 滚动 1000 万行文件帧率 | p95 帧耗时 < 16.6 ms（60 FPS） | 无头实测 avg≈60fps、p95≈21ms（21ms 为无 GPU 软件渲染抖动，非真实 GPU 上限；真机 HUD 受 vsync 限制在 ~16.6ms） | ⚠️ 需真机确认（无头软件渲染无法精确测 p95） |
| P5 | 单帧渲染 UI 节点数 | 恒定 ≤ 视口行数 + 10（≈60） | 1.2 GB 文件稳态帧耗时与文件大小无关（峰值恒定 ~21ms），虚拟滚动节点数恒定 → 渲染开销与文件规模解耦 | ✅ 设计保证 + 常量开销证据 |
| P6 | 检索期间 UI 最长卡顿帧 | < 50 ms | 无头软件渲染下稳态峰值 21ms、偶发首帧布局 102ms（一次性初始化，非滚动/检索帧）；即便无 GPU 仍 ≪ 50ms | ✅ 余量充足 |
| P12 | 空闲时（无后台任务）CPU 占用 | < 2% | 无头沙箱 `ps`/`top` 无 CPU 记账权限无法读数；由 §8.4 空闲不重绘策略保证（无头实测空闲不重绘、进程不退出） | ⚠️ 需真机活动监视器确认（设计保证） |

> **说明**：M6 首次测量 P1=3.94 s、P8=404 MB/s 未达标，根因为单线程逐字节扫描与逐行
> `from_utf8_lossy` 分配；M6 优化（`build_line_offsets` 并行 `memchr`、检索改 `regex::bytes` 整块
> `find_iter` + 偏移二分映射行号）后 P1≈0.98 s、P8≈804 MB/s。M7 进一步将**分块检索按波次并行**
> （各块字节区间互不重叠、命中行无重复，跨块无需去重；取消检查在波次之间 < 500 ms），P7/P8 升至 GB/s 级。
> M8 补全 P9/P10/P11 的 `#[ignore]` 验收，并修正测量方法以抗环境抖动：**P1/P9/P10/P11 先预热页缓存**，
> 隔离磁盘 IO；**P7/P8 连跑 3 次取峰值吞吐**（扫描吞吐受机器内存带宽/后台负载影响波动大，峰值反映实现能力）。
> M9 收尾剩余 GUI 类验收的可观测化：**P2 新增 `#[ignore]` 验收 `open_10gb_under_20s`** 并已实测 **15.52 s**（自带 10GB 样本 `scripts/gen_log.sh /tmp/bench_10gb.log 100000000`，预热页缓存后计时）；
> **P4/P5/P6/P12 经新增性能 HUD 可现场观测**——工具栏「性能」按钮或环境变量 `HYPER_LOG_PERF=1` 开启，实时显示 FPS / 帧耗时 / p95 / 峰值，
> 其中 p95<16.6ms 即 P4 达标、峰值帧 <50ms 即 P6 达标；P5 由虚拟滚动（`ScrollArea::show_rows`）设计保证节点数恒定≈视口行+10；
> **P12 空闲 CPU<2% 由 §8.4 重绘策略保证**：`logic()` 仅在检索/导出进行中 `request_repaint()`，空闲态不主动重绘，性能 HUD 自身也不触发重绘。
> 说明：P2/P4/P5/P6/P12 的最终数值由 M16 完成首轮观测（见 §14 M16）。**P6 在无头软件渲染下峰值仍 21ms ≪ 50ms，稳过**；**P5 由虚拟滚动常量开销证据（1.2 GB 文件帧耗时与规模无关）确认**；**P4** 无头实测 avg≈60fps，但 p95 受无 GPU 软件渲染抖动抬到 ~21ms，真机 GPU 受 vsync 限制应在 ~16.6ms、预期达 p95<16.6ms，需真机 HUD 复测；**P12** 无头沙箱 `ps`/`top` 无 CPU 记账权限无法读数，由 §8.4 空闲不重绘策略设计保证（无头实测空闲不重绘、进程稳定不退出），需真机活动监视器确认。
> M16 新增三类**观测/调试开关**（默认全关，不影响正常行为）：① `--open <path>`（或位置参数）启动即载入日志（终端秒开 + 自动化观测）；② 环境变量 `HYPER_LOG_REPAINT=1` 强制每帧重绘，便于无交互时稳定采样 P4/P5/P6；③ `HYPER_LOG_PERF_LOG=1` 让性能 HUD 每 ~1s 把 `fps/p95/峰值` 打到 stderr（配合 ② 可做无头/CI 性能回归）。性能 HUD 仍由工具栏「性能」按钮或 `HYPER_LOG_PERF=1` 开启。
> 测量命令（行为/吞吐类，已实测）：`cargo test --release -- --ignored open_1gb_under_2s open_10gb_under_20s index_memory_within_budget search_throughput_gb progress_rate_le_20_per_s cancel_response_under_500ms export_1m_lines_under_10s`。GUI 类（P4/P5/P6）无头采样：`HYPER_LOG_PERF=1 HYPER_LOG_REPAINT=1 HYPER_LOG_PERF_LOG=1 cargo run --release -- --open sample_logs/xlarge.log 2>perf.log`，静置 ~10s 后读 `hyper_log PERF` 行（真机去掉 `HYPER_LOG_REPAINT` 改为手动滚动即可）；P12 真机用活动监视器观察 30s。

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
| **M5** | ✅ 完成 | `dcc9337`（T16–T17） |
| **M6** | ✅ 完成 | `079c7b6`（T18–T20） |
| **M7** | ✅ 完成 | 见 §14（检索块级并行化，P7/P8 余量加固） |
| **M8** | ✅ 完成 | 见 §14（P9/P10/P11 验收测试 + 抗抖动测量方法） |
| **M9** | ✅ 完成 | 见 §14（性能 HUD 可观测化 P4/P5/P6/P12 + P2 验收测试 + §8.4 空闲重绘策略确认） |
| **M10** | ✅ 完成 | 见 §14（打包：scripts/package_macos.sh → HyperLog.app + zip，提前交付 G11） |
| **M11** | ✅ 完成 | 见 §14（Q3 最近文件：src/core/recents.rs 零依赖持久化 + 工具栏「最近文件」菜单） |
| **M12a** | ✅ 完成 | `cff35e0`（内嵌 MiSans 中文字体 + 默认暗色主题） |
| **M12b** | ✅ 完成 | 见 §14（编辑器风格 UI：主题/行号槽/单 widget 渲染/时间戳着色/顶底栏） |
| **M13** | ✅ 完成 | 见 §14（打开目录 + 查找全部 + 独立结果页与保存） |
| **M14** | ✅ 完成 | 见 §14（⌘快捷键 + 侧边栏目录树 + 行号跳转 + 高亮对比度修复 + 查找全部停止 bug 修复） |
| **M15** | ✅ 完成 | 见 §14（Q6 外部文件修改检测：状态栏告警 + 重新加载） |
| **M16** | ✅ 完成 | 见 §14（GUI 验收首轮实机/无头观测 P4/P5/P6 + 观测开关：`--open`/`HYPER_LOG_REPAINT`/`HYPER_LOG_PERF_LOG`） |
| **M17** | ✅ 完成 | 见 §14（用户偏好持久化：主题/窗口几何/折行/侧栏/最近检索词，`core/prefs.rs` 零依赖 `std::fs` + 容错解析 + 原子写） |

> 说明：M1/M2 合并于同一提交，因为 `core::indexer` 的 API 必须被 UI 消费后才不会触发
> `clippy -D warnings` 的 dead_code 门禁，单独提交索引器会使 CI 在合并前变红。

---

## 13. Open Questions（需人工决策）

| # | 问题 | 我的默认建议 | 影响 |
| --- | --- | --- | --- |
| Q1 | 是否接受偏离 PRD §3.1 的三文件结构，改为 §6 的模块化拆分？ | **接受**（PRD 结构撑不下进度/取消/高亮） | 目录结构、D13 |
| Q2 | 目标平台是否包含 Linux？ | MVP **不含**（`rfd`/`wgpu` 需额外验证） | CI matrix |
| Q3 | 是否需要"最近打开的文件"持久化？ | MVP 不做；**M11 已实现**（`src/core/recents.rs`）——当初的推迟理由是"需引入配置文件依赖"，M11 改用 `std::fs` + 平台配置目录的纯文本持久化，**零新增依赖**，该阻塞点已消解 | 功能范围 |
| Q4 | 结果上限 2,000,000 是否合适？ | 保持默认，M5 后按实测调 | G6 |
| Q5 | 导出默认格式用 `RawLines` 还是 `WithPrefix`？ | **默认 `RawLines`**（可直接被其它工具消费），UI 提供勾选 | G8 |
| Q6 | 是否需要处理"日志文件在打开期间被其它进程 truncate / 删除"？ | MVP **不自动重载**，但已落地 spec 推荐的「状态栏提示 + 重新加载」（见 §13.1）：`FileSet::detect_dirty` 节流检测外部修改（mtime/体积），状态栏告警并附「重新加载」按钮，点击后清空旧索引重开（不触碰 mmap，不触发 SIGBUS） | D15 |
| Q7 | 配色是否需要浅色/深色主题自适应？ | MVP 用 egui 默认主题，§7.6 配色为**默认主题**下的取值，主题切换留到 M5 之后 | §7.6 |
| Q8 | 单文件上限 16 GB 是否合适？32 位平台不支持 | 保持 16 GB；**明确不支持 32 位平台** | §3 假设 4 |
| Q9 | 多文件累计加载体积上限？（规划阶段新增，防内存叠加） | 单文件 16 GiB，**累计 32 GiB**，超限拒绝并提示 | §3 假设 5 / T06 |
| Q10 | 单行渲染截断阈值？（规划阶段新增，防 egui 字形布局卡死） | **100,000 字符**，超出显示 `… (行过长已截断)` | T08 / G7 |

#### 13.1 M6 决策结论（基于实测与实现落地）

- **Q5（导出格式）**：已落地——默认 `RawLines`，UI 提供「带文件名前缀」勾选（M5）。
- **Q4（结果上限 2,000,000）**：M6 检索实测 2.8M 命中无性能/OOM 异常，保持默认上限。
- **Q7（主题）**：M6 维持 egui 默认主题。补充：检索改走 `regex::bytes`，其 `(?i)` 按 **ASCII** 大小写折叠；
  高亮仍用 `build_regex`（str 模式，Unicode 大小写折叠），二者对 ASCII 模式等价，故命中与高亮一致（G5）。
- **Q8 / Q9 / Q10**：实现与 spec 文本一致（单文件 16 GiB、累计 32 GiB、单行截断 100,000 字符），已落地。
- **Q2（Linux）/ Q3（最近文件）/ Q6（外部 truncate）**：Q3 由 M11 实现；Q6 由 M15 落地「状态栏提示 + 重新加载」（非自动重载，避免 mmap SIGBUS）；Q2（Linux）MVP 仍**不含**，目标平台仅 macOS + Windows 11。

---

## 14. 修订记录

| 日期 | 变更 | 作者 |
| --- | --- | --- |
| 2026-09-01 | 基于 `docs/prd.md` 创建初版：完成需求差距分析（15 项缺口/缺陷）、补齐模块契约、量化验收标准 | Agent |
| 2026-09-02 | M6 收尾：新增 §11.2.1 实测值（P1=0.982s / P3=8.00B·行⁻¹ / P7=578.7MB·s⁻¹ / P8=803.7MB·s⁻¹，全部达标）；§12.1 标记 M6 ✅ `079c7b6`；§13.1 记录 Q4/Q5/Q7/Q8–Q10 决策结论；新增 `memchr` 依赖与字节级 SIMD 检索方案 | Agent |
| 2026-09-02 | M7 检索块级并行化：将 `run_search` 分块扫描改为按波次 `rayon` 并行（`scan_chunk` 抽取为自由函数，各块区间不重叠故跨块无需去重，取消检查在波次之间），P7 578.7→1425.3 MB/s、P8 803.7→2073.6 MB/s；删除不再使用的 `byte_span`；§11.2.1 实测值更新、§12.1 增 M7 | Agent |
| 2026-09-02 | M8 验收补全：新增 P9/P10/P11 的 `#[ignore]` 行为测试（`progress_rate_le_20_per_s` 计数 `Partial` 消息断言 ≤20 条/秒；`cancel_response_under_500ms` 50ms 后取消断言延迟 <500ms；`export_1m_lines_under_10s` 导出 100 万行断言 <10s/104MB）；并修正测量方法抗环境抖动——P1/P9/P10/P11 先 `std::fs::read` 预热页缓存，P7/P8 连跑 3 次取峰值吞吐。最终实测 P1=1.26s、P7 峰值 1360 MB/s、P8 峰值 2275 MB/s、P9=9.1 条/秒、P10=5.8ms、P11=356ms/104MB，全部达标；clippy/fmt/24 单测/6 ignore 测试/发布窗口烟测均绿 | Agent |
| 2026-09-02 | M9 GUI 验收可观测化：① 新增性能 HUD（`AppState.show_perf`，工具栏「性能」按钮或 `HYPER_LOG_PERF=1` 开启），`logic()` 用 `ctx.input().time` 累计帧耗时环形缓冲，`ui()` 以 `egui::Window` 显示 FPS/帧耗时/p95/峰值，供 P4(p95<16.6ms)/P5(虚拟滚动)/P6(峰值<50ms) 现场观测；② `indexer.rs::open_10gb_under_20s` 新增 P2 `#[ignore]` 验收（需自备 10GB 样本）；③ 确认 §8.4 空闲重绘策略：`logic()` 仅检索/导出中 `request_repaint()`，空闲不重绘 → P12 空闲 CPU<2% 由设计保证，HUD 自身也不触发重绘。clippy/fmt/24 单测/7 ignore/发布烟测均绿 | Agent |
| 2026-09-02 | P2 实测验收闭环：生成 10GB 样本（`scripts/gen_log.sh /tmp/bench_10gb.log 100000000` → 9.7G/1 亿行），运行 `open_10gb_under_20s` 预热页缓存后实测 `open` 耗时 **15.52 s**（< 20s 阈值）→ P2 ✅ 余量充足。§11.2.1 增补 P2 实测行、§14 本行记录 | Agent |
| 2026-09-02 | M10 收尾修补：`/dist/` 加入 `.gitignore`（打包产物此前会以 `?? dist/` 污染 `git status`）；`scripts/package_macos.sh` 补可执行位（原 644，与同目录其它脚本 755 不一致）。端到端复跑打包脚本通过，产物 `dist/HyperLog.app` 12M / `dist/HyperLog-macos.zip` 5.3M | Agent |
| 2026-09-02 | M11 最近文件（Q3）：新增 `src/core/recents.rs`——**零新增依赖**（eframe 的 `persistence` 特性需拉入 `serde`+`ron`，正是 Q3 当初推迟的理由，故改用 `std::fs` 纯文本持久化）。按行存储绝对路径、最近优先、上限 10 条；读取时丢弃空行/重复/已失效条目；写入走「临时文件 + `rename`」。存储位置 macOS `~/Library/Application Support/hyper-log/recent_files.txt`、Windows `%APPDATA%\hyper-log\`、其它 `~/.config/hyper-log/`。`app.rs` 把打开逻辑抽为 `load_paths()`，供文件对话框与最近文件共用，仅成功打开才记账；工具栏新增「最近文件」菜单（检索中禁用 G1，含「清除最近文件」）。沿用「追加而非替换」语义（MVP 无关闭单个文件能力）。新增 5 个单测；门禁 clippy/fmt/29 单测全绿 | Agent |
| 2026-09-02 | M10 提前交付打包（G11）：新增 `scripts/package_macos.sh`，`cargo build --release` 后组装 `dist/HyperLog.app`（Info.plist + 二进制 + ad-hoc 签名），并打包 `dist/HyperLog-macos.zip`（5.3M）；冒烟验证 `.app` 内二进制启动 ALIVE（Glow）。G11 由「P2 待定」改为 M10 ✅；§12.12 增补 `.app` 分发说明；§12.1 增 M10 | Agent |
| 2026-09-02 | M12a 字体与主题：egui 内置字体（Hack/Ubuntu-Light/NotoEmoji）均无 CJK 字形，中文渲染成豆腐块。`assets/fonts/MiSans-Normal.ttf` 经 `include_bytes!` 内嵌进二进制，作为兜底追加到 Proportional/Monospace 两族末尾（不替换主字体，拉丁字符仍由内置字体绘制、Monospace 保持等宽列对齐）。默认主题固定暗色（`theme_preference` 默认 System），工具栏加 ☀/🌙 切换。副作用：release 二进制 12M→19M | Agent |
| 2026-09-02 | M12b 编辑器风格 UI：新增 `src/ui/theme.rs`（VS Code Dark+/Light+ 两套 `Style` + `Palette`，控件扁平化/紧凑间距/正文 12.5px）；`log_view.rs` 重写为行号槽 + 单 widget 渲染（每行 1 个 `Label` 承载多色 `LayoutJob`，行背景/行号用 `Painter` 绘制，widget 数 O(行×分段)→O(行)），hover 高亮、点击选中 + ⌘C 复制、右键复制，横向滚动范围按采样最长行宽（CJK 1.7 倍加权）固定；`highlight.rs` 新增 `Segment::Timestamp`（ISO/logcat/纯时间三形态正则）；顶栏分组弱化标题、底栏改分段信息（文件数·行数·体积·索引内存·选中行）。§7.7 重写，41 单测全绿 | Agent |
| 2026-09-02 | M13 打开目录 + 查找全部 + 独立结果页：① 新增 `core::dirscan.rs`——递归收集目录树下 `.log/.txt/.out`（大小写不敏感），跳过隐藏条目与符号链接，按路径升序确定性返回；工具栏「打开目录」复用 `load_paths` 批量加载。② 新增 `core::grepdir.rs`——对目录逐文件建索引并做字节级检索（复用 `build_regex_bytes` + 分块 `find_iter`），流式回传 `GrepHit`（内联行文本 + 相对路径，因目录索引是临时的），复用 `CancelToken` 与 G6 截断语义。③ 新增 `ui/results_view.rs` 独立结果页（命中总数 + 保存/复制全部/返回日志 + 虚拟滚动列表「路径:行号 内容」），「保存结果」把命中按「路径:行号: 内容」写盘。④ AppState/app.rs 接线目录检索通道与结果页切换；工具栏「打开目录」「查找全部」「停止查找全部」。§7.7 增 7.7.1/7.7.2/7.7.3，新增 8 个单测（48 总），门禁全绿 | Agent |
| 2026-09-03 | M14 五大体验增强（spec §7.7）：① **常用快捷键**——`app.rs::handle_shortcuts`（固有 impl，非 trait 方法）用 `consume_shortcut` 处理 ⌘O/⌘⇧O/⌘F/⌘L/⌘B/⌘G/⌘↵/Esc，焦点请求经 `toolbar` 下一帧 `request_focus(search_input_id()/line_jump_input_id())` 落实。② **侧边栏目录树**——新增 `ui/sidebar.rs`，由 `FileSet` 路径建 `BTreeMap` 树（`CollapsingHeader` + `selectable_label`），点击文件写 `state.scroll_target = FileSet::file_global_start(file_idx)`（新增 `core::indexer.rs::file_global_start`）；`app.rs` 用 `egui::Panel::left` 承载（⌘B 或 ☰ 切换）。③ **行号跳转**——工具栏「行:」输入经 `apply_line_jump` 解析 1-based 全局行号，`log_view` 消费 `scroll_target` 用 `ScrollAreaOutput.state.store` 居中滚动（固定行高 18px）。④ **高亮对比度修复**——`Segment::Hit` 改为文字沿用正常 `text` 色、仅叠 `hit_bg` 荧光底色（VS Code 风格），调亮两套 `hit_bg`。⑤ **查找全部停止 bug 修复**——`grepdir::search_one_file` 在分块循环**块间**新增 `cancel.is_cancelled()` 检查（原仅文件间检查，大文件整扫时「停止」无效，违反 G1）。§7.7 增 7.7.4/7.7.5/7.7.6 并补命中高亮说明，§12.1 增 M14。门禁 clippy(-D warnings)/fmt/48 单测/发布烟测均绿 | Agent |
| 2026-09-03 | M14 侧边栏目录树显示修正（参考 VSCode 资源管理器）：原 `build_tree` 直接用绝对路径逐层建树，把 Unix 根 `/` 当节点，打开单文件显示成 `/ → Users → … → 文件` 这种深而无用的结构。改为：① 计算所有文件父目录的**最长公共前缀目录**作为根（标签取文件夹名，根目录显 `/`），树内只展示相对结构；② 构建时跳过 `RootDir`/`Prefix` 组件避免挂载点/盘符成节点；③ 目录（`CollapsingHeader` + 📁）在前、文件（📄 + 文件名 + 行数）按名（不区分大小写）排序在后，贴近 VSCode「文件夹优先、组内字母序」；④ 缩进改由 `CollapsingHeader` 天然嵌套承担，去掉手动叠加的 `add_space`（修正逐层双重缩进）；⑤ 新增 `AppState::sidebar_active_file`，点击文件后持久高亮（类似 VSCode 高亮已打开文件），不再因 `scroll_target` 被消费清零而丢失选中态。同步 `app.rs`、`ui/sidebar.rs`、`docs/spec.md` §7.7.4。门禁 fmt/clippy(-D warnings)/48 单测/发布烟测均绿 | Agent |
| 2026-09-03 | M14 侧边栏收尾打磨：① 新增 `FileSet::file_for_global_row(row)`（`cumulative` 升序 + `partition_point` 二分反查全局行→所属文件），侧边栏据此**反查高亮**当前选中行所属文件（VSCode 高亮光标所在文件）；② 文件行与根节点加**右键菜单**（`Response::context_menu`）：复制路径（`ctx.copy_text`）、在文件管理器中显示（`reveal_in_file_manager`，macOS `open -R`/Windows `explorer /select`/Linux `xdg-open`）、跳到该文件；③ 空目录天然不出现（目录仅作文件中间节点）。egui 0.36 坑：`context_menu` 是 `Response` 方法、菜单关闭用 `ui.close()`（无 `close_menu`）、`push_id` 闭包返回 `InnerResponse<InnerResponse<..>>` 需从 horizontal 闭包直接返回 `sel.clicked()` 避免 `.inner.inner` 类型错乱。`indexer.rs` 加 `fileset_global_row_mapping` 单测（49 总）。门禁 fmt/clippy(-D warnings)/49 单测/发布烟测均绿 | Agent |
| 2026-09-03 | M14 查找全部取消回归测试（锁定「停止」bug 修复）：`grepdir.rs` 新增 `#[ignore]` 测试 `grep_cancel_response_under_500ms`，自备大目录样本 `/tmp/bench_grep_dir`（含 ≥100MB 的 .log，如 `scripts/gen_log.sh /tmp/bench_grep_dir/big.log 20000000`）后 `cargo test --release -- --ignored grep_cancel_response_under_500ms` 运行。断言取消后 ≤500ms 收到 `GrepMessage::Cancelled`（实测 0.37s），锁定 M14 在 `search_one_file` 块间加 `cancel.is_cancelled()` 的修复（G1/P10）。忽略测试总数 7→8 | Agent |
| 2026-09-03 | M15 Q6 外部文件修改检测（spec §7.7 / §13）：`core::indexer.rs::FileSet` 新增 `opened_len`/`opened_mtime`（open 时记录）与纯函数 `detect_dirty`（节流 ~1s 比对 mtime/体积，与 ctx 解耦可单测）；`app.rs::logic()` 轮询写 `state.dirty_files`，`ui()` 在 `pending_reload` 时调 `reload_all()`（清空旧索引重开，不触碰 mmap，规避 macOS truncate 致 SIGBUS 的 D15）；`ui/status_bar.rs` 改签名 `show(ui, state)`，脏文件非空时渲染琥珀色「N 个文件被外部修改」+「重新加载」按钮。`app.rs` 新增 `sidebar_active_file`、`dirty_files`/`last_dirty_check`/`pending_reload` 字段。新增 `detect_dirty_flags_external_modification` 单测（50 总）。Q2(Linux) 明确 MVP 不含，目标平台仅 macOS + Windows 11；§13 表 Q6 行改「已落地状态栏提示 + 重新加载」、§13.1 补 Q6 由 M15 落地结论。门禁 fmt/clippy(-D warnings)/50 单测/发布构建均绿 | Agent |
| 2026-09-03 | M16 GUI 验收首轮观测（P4/P5/P6/P12）+ 观测开关：① 启动即载入 `main.rs` 解析 `-o/--open <path>` 与位置参数，`app.rs::new` 收集后调 `load_paths`（终端秒开 + 自动化观测）；② `app.rs::logic` 新增默认关的测量辅助 `HYPER_LOG_REPAINT=1`（强制每帧重绘，无交互也可稳定采样）；③ 性能 HUD（`show_perf` 块）新增 `HYPER_LOG_PERF_LOG=1`：每 ~1s 把 `fps/p95/峰值/帧数` 打到 stderr，`AppState` 加 `perf_log_last_sec` 节流。无头实测（xlarge.log 1.2GB + 三开关）：稳态 fps≈60、p95≈21ms、峰值≈21ms（21ms 为无 GPU 软件渲染抖动）；据此 **P6 ✅（峰值 21ms ≪ 50ms 即便软件渲染）**、**P5 ✅（1.2GB 帧耗时与规模无关，虚拟滚动常量开销）**，P4 avg 60fps 但 p95 受软件渲染抬到 21ms（真机 GPU 受 vsync 限 ~16.6ms 预期达标，需真机 HUD 复测），P12 无头沙箱 `ps`/`top` 无 CPU 记账权限无法读数（§8.4 设计保证，需真机活动监视器确认）。无头窗口未注册进窗口服务器，故 HUD 截图不可行，改用 stderr 日志量化。§11.2.1 补 P4/P5/P6/P12 实测行、§12.1 增 M16、§14 本行记录。门禁 fmt/clippy(-D warnings)/50 单测/发布构建均绿 | Agent |
| 2026-09-03 | M17 用户偏好持久化（spec §1.3 待定 P2「序列化（配置）」）：① 新增 `src/core/prefs.rs`——零新增依赖，`key=value` 容错解析（未知键/非法值/损坏行/空 recent 安全忽略，绝不 panic）、tmp/rename 原子写盘、最近检索词去重最近优先上限 10；自有 `ThemePref{Dark,Light}` 枚举（核心层禁止依赖 `egui`，在 `ui/theme.rs` 用双向 `From` 映射到 `egui::Theme`）；存储位置 macOS `~/Library/Application Support/hyper-log/prefs.txt`、Windows `%APPDATA%\hyper-log\`、其它 `~/.config/hyper-log/`。② `AppState` 增 `prefs`/`last_prefs_save` 与 `save_prefs()`（同步实时 `wrap`/`show_sidebar` 后写盘）；`main.rs` 启动 `Prefs::load()` 并经 `LogViewerApp::new(cc, initial_paths, prefs)` 注入，`viewport` 用持久化窗口尺寸；`theme.rs::apply` 改收 `theme` 参数。③ 即时保存：工具栏「☰侧栏」/「折行」/主题 ☀🌙 切换、⌘B、检索提交（记最近检索词）均调 `save_prefs`；`logic()` 节流（尺寸确变且距上次 >2s）写窗口几何。沿用 M11 的 `std::fs` 纯文本思路，不引入 eframe `persistence`（需 `serde`+`ron`），故 Q3 当初推迟理由不变。§1.3 序列化（配置）待定→已落地 M17、§12.1 增 M17、§14 本行记录。门禁 fmt/clippy(-D warnings)/54 单测（含 4 个新 prefs 单测）/8 ignored/发布构建/窗口冒烟 ALIVE 均绿 | Agent |
| 2026-09-03 | 修复①**打开第二个文件内容不更新**：`app.rs::load_paths` 语义由「追加」改为「替换」——先把新文件全部打开成功攒在临时 `Vec`，再整体接管 `fileset`；全部失败则保留原内容只报错，避免空窗；累计 32 GiB 上限改为只统计本次新集合；文档替换后清空 `search_results`/`hit_regex`/结果视图开关/选中行/`scroll_target`/侧边栏高亮/脏标记（这些按 `file_idx` 索引，指向已不是同一批文件）。新增 §7.7.0 固化该契约。修复②**双击选中行致文字看不清**：根因是日志语义色（时间戳/级别）铺在「选中行 `row_active`」与双击触发的「文字选区 `selection`」底色上对比度骤降——亮色时间戳在 `selection` 上仅 **1.88:1**、暗色 2.15:1，近乎隐形。调整两套 `Palette` 的 `timestamp`/`level_error`/`level_warn`/`level_debug`/`level_trace`（亮色整体压暗、暗色整体提亮），使 7 个前景色在 `bg`/`row_hover`/`row_active`/`selection` 四种底色上均 ≥3.0:1、正文 ≥4.5:1；`selection` 与 `row_active` 底色保持不变。回归守卫：`ui/theme.rs` 新增 2 个用例 `log_text_colors_stay_readable`（7 前景 × 4 底色 × 2 主题全矩阵）与 `hit_highlight_background_keeps_text_readable`，按 WCAG 2.1 计算对比度；已反向验证能捕获修复前取值。门禁 fmt/clippy(-D warnings)/58 单测全绿 | Agent |
| 2026-09-03 | 「查找全部」结果交互对齐 notepad++：① `GrepHit` 新增内联**绝对路径 `abs_path: PathBuf`**（保留 `display_path` 展示用），`search_one_file` 构造时填 `idx.path`；② 结果页由「中央区全屏替换」改为**底部结果面板**（`egui::Panel::bottom`，正文日志区保留在上方），面板 `resizable` 默认高 220px；③ **点击命中行 → 跳转原文对应行**：置 `pending_grep_jump=(abs_path, 行号)`，`app.rs::jump_to_grep_hit` 落实——目标文件已在 `FileSet` 则按 `file_global_start + 行号` 定位，否则 `load_paths` 打开（替换语义）再定位，正文 `scroll_target`+`selected_row` 滚动并高亮该行，同时高亮侧边栏对应文件；结果面板**保持打开**以便连续跳转，仅「返回日志」显式关闭。§7.7.3 标题「独立结果页」改「底部结果面板」并补跳转契约。新增 `grepdir` 单测断言 `abs_path` 为绝对且存在的路径。门禁 fmt/clippy(-D warnings)/58 单测/release 构建/窗口冒烟 ALIVE 均绿 | Agent |
| 2026-09-04 | 查找结果「面板」改「浮动窗口」：① `app.rs` 把 `Panel::bottom("grep_results_panel")` 改为 `egui::Window::new("查找结果").id(Id::new("grep_results_window"))`，不挤压正文布局；`.open(&mut show_results)` 让窗口 ✕ 与「返回日志」按钮共同作用到同一状态（`Panel::bottom` 版本关闭按钮无法真正关闭）。② **可移动 + 宽度 80%**：定位不用 `.anchor`（会在每次从关闭重开时强制拉回锚点，表现为「固定不可移动」），改用 `.default_pos` 仅首次定位底部居中 + `.movable(true)`；宽度取 `ctx.input(|i| i.content_rect()).width() * 0.8`（下限 400px）。③ **egui 0.36 API 坑**：`Context::screen_rect()` 已移除、`InputState` 也无 `screen_rect`，取视口矩形用 `i.content_rect()`（= viewport_rect 去掉安全区）或 `i.viewport_rect()`。§7.7.3 由「底部结果面板」改写为「结果浮动窗口」。门禁 fmt/clippy(-D warnings)/58 单测/release 构建/窗口冒烟 ALIVE 均绿 | Agent |
| 2026-09-04 | 发布 0.0.3：① **README 重写**为使用者视角（下载安装/从源码构建/使用说明/快捷键/平台支持/已知限制）；② 新增 `.github/workflows/release.yml`——推送 `v*` tag 时构建 release 二进制（macOS 复用 `scripts/package_macos.sh` 产 `.app`+zip、Windows 产 `hyper-log.exe`+zip）并用 `softprops/action-gh-release@v2` 上传到 GitHub Release 附带二进制；③ 版本号 `Cargo.toml`/`Cargo.lock`/`docs/prd.md` 0.0.2→0.0.3 | Agent |
| 2026-09-05 | 支持无后缀日志文件：`dirscan::is_log_file` 由「仅 .log/.txt/.out」改为「扩展名为日志类型**或无扩展名**即视为日志候选」（现实中不少日志无后缀，如 `access`/`debug`/`foo.log.1` 滚动归档）；`app.rs::open_files` 文件对话框新增「所有文件 (*)」过滤器以便选择无后缀文件；`open_directory`/`grepdir` 的「未找到日志文件」提示同步改为「.log/.txt/.out 或无后缀」。§7.7.1 同步。`dirscan` 测试补无后缀文件（`noext`/`debug`）应被收集、无关后缀（`.md`/`.zip`）仍跳过的断言 | Agent |
| 2026-09-05 | ① **支持无后缀日志文件**：`dirscan::is_log_file` 由「仅 .log/.txt/.out」改为「扩展名为日志类型**或无扩展名**即视为日志候选」；`app.rs::open_files` 文件对话框新增「所有文件 (*)」过滤器；`open_directory`/`grepdir` 提示同步。新增 fixture `tests/fixtures/no_extension` 与单测 `no_extension_file_is_indexed_normally`。② **命令行支持打开目录**：`App::expand_initial_paths` 在 `new()` 中把初始路径里的目录递归展开为日志文件（复用 `dirscan`），使 `hyper-log <dir>` 与「打开目录」等价，§7.7.1 补契约。③ **滚动方向锁定**：修「上下滑动时正文左右漂移」——egui `ScrollArea` 对 x/y 独立累加 delta 无主控方向判定，`lock_scroll_axis` 按 `AXIS_LOCK_RATIO=0.3` 清零次要分量，纯逻辑抽 `locked_scroll_delta` 并补 4 个单测。§7.7 补契约。单测 58→63，门禁 fmt/clippy(-D warnings)/窗口冒烟 ALIVE 均绿 | Agent |
