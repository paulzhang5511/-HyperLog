# Hyper Log

一款面向**开发 / 运维 / 测试**的桌面日志查看器。用 Rust + egui 打造，主打一个「快」——**秒级打开 GB 级日志**、**千万行流畅滚动**、**不冻结界面**的跨文件正则检索与结果导出。

- 🚀 **秒开大文件**：内存映射（mmap）零拷贝读取，1 GB 日志约 1 秒即可打开并滚动。
- 🖱️ **千万行流畅滚动**：虚拟滚动只渲染视口内的行，行数再多也不卡。
- 🔍 **流式检索**：后台线程分块并行匹配，边搜边出结果，真实百分比进度，随时取消。
- 📦 **流式导出**：命中结果按原始字节惰性写出，与源文件逐字节一致（含无效 UTF-8 行）。
- 🎨 **级别高亮**：ERROR / WARN / INFO / DEBUG 按级别着色，命中片段荧光高亮。
- 🗂️ **多文件与目录**：同时打开多个文件，或一键打开整个目录下的日志。
- 🖥️ **跨平台**：macOS 与 Windows，自带中文字体，暗 / 亮双主题。

---

## 下载安装

无需编译，直接到 [Releases](https://github.com/paulzhang5511/-HyperLog/releases) 下载对应平台的打包二进制：

| 平台 | 产物 | 说明 |
| --- | --- | --- |
| macOS（Apple Silicon / Intel） | `HyperLog-macos.zip` | 解压后把 `HyperLog.app` 拖入「应用程序」 |
| Windows（x64） | `hyper-log-windows.zip` | 解压后双击 `hyper-log.exe` |

> macOS 首次打开若提示「无法验证开发者」，请右键 →「打开」，或到「系统设置 → 隐私与安全性」点击「仍要打开」。

---

## 从源码构建

> 性能务必在 **release** 下测量：`dev` 构建的正则匹配慢 10~50 倍，数据无参考价值。

```bash
# 开发 / 调试
cargo run

# 发布构建（推荐，opt-level=3 + thin LTO）
cargo build --release
./target/release/hyper-log
```

渲染后端默认 **glow（OpenGL）**——部分 macOS 机型上 `wgpu` 会因 Metal 着色器编译失败而无法启动。如需切换：

```bash
HYPER_LOG_RENDERER=wgpu ./target/release/hyper-log   # 强制 wgpu
HYPER_LOG_RENDERER=glow ./target/release/hyper-log   # 强制 glow（默认）
```

日志通过 `env_logger` 输出，`RUST_LOG` 控制级别：

```bash
RUST_LOG=debug ./target/release/hyper-log
```

---

## 使用说明

### 打开日志

- 点工具栏「**打开**」选择**一个或多个**日志文件（单文件 ≤ 16 GiB，累计 ≤ 32 GiB）。
- 点「**打开目录**」递归加载目录下所有 `.log` / `.txt` / `.out` 文件。
- 也可在命令行直接指定文件秒开：`hyper-log /path/to/app.log`，或 `hyper-log --open /path/to/app.log`。

打开新文件即**切换**当前文档（与编辑器一致），左侧「目录树」（☰）展示已打开文件的结构。

### 检索

1. 在检索框输入关键字或正则，选择「纯文本 / 正则」与「大小写敏感」（`Aa`）。
2. 点「**查找**」在当前文档检索，命中行在「仅命中」视图下浏览（命中词高亮）。
3. 点「**查找全部**」对目录递归检索，结果在**底部浮动窗口**「查找结果」中列出，
   点击任意命中行即**跳转到原文对应行**。
4. 检索中可随时点「停止」；进度条平滑增长，命中逐批出现。

### 导出

- 检索后点「导出」，选择 `RawLines`（仅原始行）或「带前缀」（`文件名:行号:` 前缀）。
- 导出直接写源文件字节，**不做 UTF-8 往返**，含无效 UTF-8 的行也逐字节保真。

---

## 快捷键

| 操作 | macOS | Windows / Linux |
| --- | --- | --- |
| 打开文件 | `⌘O` | `Ctrl+O` |
| 打开目录 | `⌘⇧O` | `Ctrl+Shift+O` |
| 聚焦检索框 | `⌘F` | `Ctrl+F` |
| 行号跳转 | `⌘L` | `Ctrl+L` |
| 切换侧边栏目录树 | `⌘B` | `Ctrl+B` |
| 触发检索 | `⌘G` / `⌘↵` | `Ctrl+G` / `Ctrl+Enter` |
| 退出命中视图 / 清除选中 | `Esc` | `Esc` |
| 复制选中行 | `⌘C` | `Ctrl+C` |

---

## 已知限制

- **Windows mmap 锁文件**：Windows 下 mmap 会锁定被打开的日志文件，其它进程无法删除 / 覆写；关闭 Hyper Log 后释放。
- **文件被外部 truncate / 删除**：不监听文件变化；若日志在打开期间被外部截断，状态栏会告警，点「重新加载」即可。
- **不支持 32 位平台**：单文件上限 16 GiB，依赖 64 位地址空间。
- **Linux**：MVP 未验证（需要 `rfd` / 渲染后端在 Linux 的额外适配）。

---

## 性能验收

大文件指标见 `docs/spec.md` §11.2（含实测值）。性能样本生成与本地测量：

```bash
scripts/gen_log.sh /tmp/bench_1gb.log 10000000
cargo test --release -- --ignored open_1gb_under_2s index_memory_within_budget search_throughput_gb
```

---

## 项目结构

```
src/
  main.rs            窗口启动、渲染后端选择
  app.rs             LogViewerApp：状态机、logic()/ui()、后台消息轮询
  core/
    indexer.rs       LogFileIndex / FileSet：mmap + 行偏移索引
    search.rs        流式分块检索引擎 + 消息协议
    grepdir.rs       目录递归检索（「查找全部」）
    export.rs        流式导出
    dirscan.rs       目录扫描
  highlight.rs       级别 / 命中着色（纯函数）
  ui/                顶部工具栏 / 中央日志区 / 结果浮动窗口 / 底部状态栏
  util.rs            字节格式化、导出时间戳
tests/               边界 fixtures + 性能验收入口
docs/                spec.md（事实来源）/ plan.md / tasks.md
```

---

## 许可

本项目依赖均为 MIT / Apache-2.0 兼容许可证。
