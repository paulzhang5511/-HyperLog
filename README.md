# Hyper Log

桌面端高性能日志查看器。面向**开发 / 运维 / 测试**人员，能在**秒级打开 GB 级文本日志**、**千万行流畅滚动**，并在**不冻结界面**的前提下完成跨文件正则检索与结果导出。

- **Rust + egui 0.36**：即时模式 GUI，虚拟滚动，零拷贝内存映射读取。
- **流式检索**：后台线程分块并行匹配，边搜边出结果，真实百分比进度，随时取消。
- **流式导出**：检索命中按坐标惰性取原始字节写出，与源文件逐字节一致。
- **级别高亮**：ERROR / WARN / INFO / DEBUG 等按级别着色，命中片段带底色。

## 构建与运行

> 性能验收务必在 **release** 下测量：`dev` 构建的正则匹配慢 10~50 倍，数据无参考价值。

```bash
# 开发 / 调试
cargo run

# 发布构建（推荐，opt-level=3 + thin LTO）
cargo build --release
./target/release/hyper-log
```

渲染后端默认 **glow（OpenGL）**——在部分 macOS 机型上 `wgpu` 会因 Metal 着色器编译失败而无法启动。
如需切换：

```bash
HYPER_LOG_RENDERER=wgpu ./target/release/hyper-log   # 强制 wgpu
HYPER_LOG_RENDERER=glow ./target/release/hyper-log   # 强制 glow（默认）
```

日志通过 `env_logger` 输出，`RUST_LOG` 控制级别：

```bash
RUST_LOG=debug ./target/release/hyper-log
```

## 使用流程

1. 顶部「打开」选择**一个或多个**日志文件（≤ 32 个，单文件 ≤ 16 GiB，累计 ≤ 32 GiB）。
2. 检索框输入关键字或正则，选择「纯文本 / 正则」与「大小写敏感」，回车或点「搜索」。
3. 检索中可随时点「停止」；进度条平滑增长，命中逐批出现。
4. 勾选「显示命中」在结果视图下浏览命中行（命中词高亮）。
5. 点「导出结果」保存对话框，选择 `RawLines`（仅原始行）或「带文件名前缀」（`文件名:行号:` 前缀）。

## 快捷键

| 操作 | 快捷键 |
| --- | --- |
| 检索框内触发检索 | `Enter` |
| 退出应用 | `Cmd/Ctrl + Q` |
| 打开文件 | 顶部「打开」按钮（原生文件对话框多选） |

## 导出格式

- **RawLines**（默认）：仅写出命中的原始行，可直接被 `grep` / 其它工具消费。
- **WithPrefix**：每行前缀 `<文件名>:<行号>:`（行号为文件内 1-based），便于贴入缺陷单。

导出直接写入源文件字节，**不做 UTF-8 往返**，因此含无效 UTF-8 的行也能逐字节保真。

## 已知限制

- **Windows mmap 锁文件**：Windows 下 mmap 会锁定被打开的日志文件，其它进程无法删除 / 覆写；关闭 Hyper Log 后释放。
- **文件被外部 truncate / 删除**：MVP 不监听文件变化；若日志在打开期间被外部截断，请重新加载（状态栏会提示）。
- **不支持 32 位平台**：单文件上限 16 GiB，依赖 64 位地址空间。
- **Linux**：MVP 未验证（需要 `rfd` / 渲染后端在 Linux 的额外适配）。

## 性能验收

大文件指标见 `docs/spec.md` §11.2（含实测值）。性能样本生成与本地测量：

```bash
scripts/gen_log.sh /tmp/bench_1gb.log 10000000
cargo test --release -- --ignored open_1gb_under_2s index_memory_within_budget search_throughput_gb
```

## 项目结构

```
src/
  main.rs            窗口启动、渲染后端选择
  app.rs             LogViewerApp：状态机、logic()/ui()、后台消息轮询
  core/
    indexer.rs      LogFileIndex / FileSet：mmap + 行偏移索引
    search.rs       流式分块检索引擎 + 消息协议
    export.rs       流式导出
  highlight.rs      级别 / 命中着色（纯函数）
  ui/                顶部工具栏 / 中央日志区 / 底部状态栏
  util.rs           字节格式化、导出时间戳
tests/              边界 fixtures + 性能验收入口
docs/               spec.md（事实来源）/ plan.md / tasks.md
```

## 许可

本项目依赖均为 MIT / Apache-2.0 兼容许可证。
