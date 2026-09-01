//! 流式导出：把检索命中坐标逐行写出为新的日志文件（spec §7.8，修复 D8 / G8）。
//!
//! 设计要点：
//! - 结果只持有 `(file_idx, line_idx)` 坐标，导出时按坐标惰性取 `line_bytes()`，
//!   全程**不做 UTF-8 转换**，保证导出文件与源文件命中行**逐字节一致**（不变式 6）；
//! - 通过 `Arc<FileSet>` + `Arc<Vec<SearchHit>>` 共享数据，引擎内部**不 clone 大 Vec**；
//! - 8 MiB `BufWriter` 缓冲，每 10,000 行或每 200 ms 推送一次进度；
//! - `CancelToken` 可中断：取消后保留已写部分并提示用户（不清理、不 panic）。

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::core::CancelToken;
use crate::core::indexer::FileSet;
use crate::core::search::SearchHit;

/// 文件写缓冲：8 MiB（spec §7.8）。
const EXPORT_BUF_BYTES: usize = 8 * 1024 * 1024;
/// 每写入这么多行推送一次进度（spec §7.8）。
const EXPORT_PROGRESS_EVERY: usize = 10_000;
/// 至少这么久推送一次进度，避免超大单行拖慢反馈（spec §7.8）。
const EXPORT_PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// 导出格式（spec §7.8 / plan Q5）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportFormat {
    /// 仅写出命中的原始行（默认，可直接被其它工具消费）。
    #[default]
    RawLines,
    /// 每行加 `<文件名>:<行号>:` 前缀（行号为文件内 1-based，见 §7.8 / G8）。
    WithPrefix,
}

/// 导出后台 → UI 的消息（spec §7.8）。
#[derive(Debug)]
pub enum ExportMessage {
    /// 进度：`done` 已写行数，`total` 总命中行数。
    Progress { done: usize, total: usize },
    /// 完成：目标路径与写出字节数。
    Completed { path: PathBuf, bytes: u64 },
    /// 失败：原因原文（目标不可写 / 写入错误），不 panic。
    Failed(String),
    /// 已响应取消并退出；已写部分保留在磁盘上。
    Cancelled,
}

/// 流式导出命中结果到 `dest`（spec §7.8）。
///
/// 调用方负责在后台线程中 `spawn`；本函数同步执行直至完成 / 失败 / 取消。
/// 命中坐标失效（文件已被卸载）时跳过该行，不中断整体导出。
pub fn export_async(
    files: Arc<FileSet>,
    hits: Arc<Vec<SearchHit>>,
    dest: PathBuf,
    format: ExportFormat,
    cancel: CancelToken,
    tx: Sender<ExportMessage>,
) {
    // 目标可写性 / 打开：失败即 Failed，不 panic（spec §7.8）。
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&dest)
    {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(ExportMessage::Failed(format!(
                "无法写入 {}: {e}",
                dest.display()
            )));
            return;
        }
    };
    let mut writer = BufWriter::with_capacity(EXPORT_BUF_BYTES, file);

    let total = hits.len();
    let mut done = 0usize;
    let mut written: u64 = 0;
    let mut last_send = Instant::now();

    for hit in hits.iter() {
        // 取消：保留已写部分，回传 Cancelled（spec §7.8）。
        if cancel.is_cancelled() {
            let _ = writer.flush();
            let _ = tx.send(ExportMessage::Cancelled);
            return;
        }

        let fi = hit.file_idx as usize;
        let li = hit.line_idx as usize;
        // 坐标失效：文件已被卸载，跳过该行而不中断。
        let Some(file_idx) = files.file(fi) else {
            done += 1;
            continue;
        };
        let Some(bytes) = file_idx.line_bytes(li) else {
            done += 1;
            continue;
        };

        // WithPrefix：`<文件名>:<行号>:`（文件名而非序号，见 G8）。
        if format == ExportFormat::WithPrefix {
            let name = file_idx
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_idx.path.to_string_lossy().into_owned());
            let prefix = format!("{}:{}:", name, li + 1);
            if let Err(e) = writer.write_all(prefix.as_bytes()) {
                let _ = tx.send(export_failed(&dest, e));
                return;
            }
            written += prefix.len() as u64;
        }

        // 写原始行字节 + 行尾换行（line_bytes 已剥离 EOL，故补 `\n`）。
        if let Err(e) = writer
            .write_all(bytes)
            .and_then(|_| writer.write_all(b"\n"))
        {
            let _ = tx.send(export_failed(&dest, e));
            return;
        }
        written += bytes.len() as u64 + 1;
        done += 1;

        if done.is_multiple_of(EXPORT_PROGRESS_EVERY)
            || last_send.elapsed() >= EXPORT_PROGRESS_INTERVAL
        {
            let _ = tx.send(ExportMessage::Progress { done, total });
            last_send = Instant::now();
        }
    }

    if let Err(e) = writer.flush() {
        let _ = tx.send(export_failed(&dest, e));
        return;
    }
    let _ = tx.send(ExportMessage::Completed {
        path: dest,
        bytes: written,
    });
}

/// 构造导出失败消息。
fn export_failed(dest: &Path, e: std::io::Error) -> ExportMessage {
    ExportMessage::Failed(format!("导出写入失败 {}: {e}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use crate::core::indexer::LogFileIndex;

    /// 测试用临时导出目标，避免并行用例互相覆盖。
    fn tmp_dest(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("hl_export_{tag}_{}_{n}.log", std::process::id()))
    }

    const INVALID_UTF8: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/invalid_utf8.log"
    );
    const LEVELS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/levels.log");

    #[test]
    fn raw_lines_byte_identical_incl_invalid_utf8() {
        let idx = Arc::new(LogFileIndex::open(INVALID_UTF8).unwrap());
        let n = idx.line_count();
        let mut fs = FileSet::new();
        fs.push(idx.clone());

        let hits: Vec<SearchHit> = (0..n as u32)
            .map(|i| SearchHit {
                file_idx: 0,
                line_idx: i,
            })
            .collect();

        let dest = tmp_dest("raw");
        let (tx, rx) = crossbeam_channel::unbounded();
        export_async(
            Arc::new(fs),
            Arc::new(hits),
            dest.clone(),
            ExportFormat::RawLines,
            CancelToken::new(),
            tx,
        );

        // 收齐消息，确认以 Completed 结束。
        let mut done = false;
        while let Ok(m) = rx.try_recv() {
            if matches!(m, ExportMessage::Completed { .. }) {
                done = true;
            }
        }
        assert!(done, "导出应以 Completed 结束");

        let exported = std::fs::read(&dest).unwrap();
        // 期望 = 各行 line_bytes 拼接 `\n`，与源行内容逐字节一致（不变式 6）。
        let mut expected = Vec::new();
        for i in 0..n {
            expected.extend_from_slice(idx.line_bytes(i).unwrap());
            expected.push(b'\n');
        }
        assert_eq!(exported, expected, "导出字节与源行内容逐字节一致");

        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn with_prefix_uses_filename_not_index() {
        let idx = Arc::new(LogFileIndex::open(LEVELS).unwrap());
        let n = idx.line_count();
        let mut fs = FileSet::new();
        fs.push(idx.clone());

        let hits: Vec<SearchHit> = (0..n as u32)
            .map(|i| SearchHit {
                file_idx: 0,
                line_idx: i,
            })
            .collect();

        let dest = tmp_dest("prefix");
        let (tx, _rx) = crossbeam_channel::unbounded();
        export_async(
            Arc::new(fs),
            Arc::new(hits),
            dest.clone(),
            ExportFormat::WithPrefix,
            CancelToken::new(),
            tx,
        );

        let content = std::fs::read_to_string(&dest).unwrap();
        for (i, line) in content.lines().enumerate() {
            assert!(
                line.starts_with(&format!("levels.log:{}:", i + 1)),
                "第 {i} 行前缀应为 `文件名:行号:`，实际：{line}"
            );
        }

        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn cancel_sends_cancelled_and_keeps_file() {
        // 预取消令牌：导出在首行即被中断，回传 Cancelled 并保留已创建的文件。
        let idx = Arc::new(LogFileIndex::open(LEVELS).unwrap());
        let n = idx.line_count();
        let mut fs = FileSet::new();
        fs.push(idx.clone());
        let hits: Vec<SearchHit> = (0..n as u32)
            .map(|i| SearchHit {
                file_idx: 0,
                line_idx: i,
            })
            .collect();

        let dest = tmp_dest("cancel");
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = CancelToken::new();
        cancel.cancel(); // 预取消，确保取消路径被触发（不依赖竞态时序）
        export_async(
            Arc::new(fs),
            Arc::new(hits),
            dest.clone(),
            ExportFormat::RawLines,
            cancel,
            tx,
        );
        let mut cancelled = false;
        while let Ok(m) = rx.try_recv() {
            if matches!(m, ExportMessage::Cancelled) {
                cancelled = true;
            }
        }
        assert!(cancelled, "取消后应收到 Cancelled");
        // 已创建的文件保留在磁盘上（不清理、不 panic，见 §7.8）。
        assert!(dest.exists(), "取消后已写部分应保留");

        let _ = std::fs::remove_file(&dest);
    }
}
