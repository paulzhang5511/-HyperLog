//! 内存映射与行偏移索引。
//!
//! 设计目标：零拷贝地把大日志文件映射进内存，并建立「行号 → 字节偏移」索引，
//! 使任意一行的读取为 O(1) 切片、任意一行的字节为 O(1) 引用。
//! 本模块不依赖任何 GUI 库，可独立单测。

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;
use thiserror::Error;

/// 单次加载允许的最大文件体积：16 GiB。
///
/// 超过此值的文件会在 [`LogFileIndex::open`] 阶段直接拒绝，
/// 避免 mmap 在 32 位平台或地址空间受限环境下失败（见 spec §13 已知限制）。
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// UTF-8 BOM，出现时跳过首部三字节。
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// 建立索引时可能发生的错误。
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("file is empty: {0}")]
    Empty(PathBuf),
    #[error("file too large: {bytes} bytes (limit {limit})")]
    TooLarge { bytes: u64, limit: u64 },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// 单个已索引的日志文件。
#[derive(Debug)]
pub struct LogFileIndex {
    pub path: PathBuf,
    /// 私有：禁止外部持有裸切片跨越生命周期（spec §7.1）。
    mmap: Arc<Mmap>,
    /// 每行起始字节偏移（相对于整个 mmap，已含 BOM 跳过），长度 == 总行数。
    line_offsets: Vec<usize>,
}

/// 多文件集合：把「文件 i 的第 j 行」统一映射为 `0..total_lines` 的全局行号。
#[derive(Clone, Debug)]
pub struct FileSet {
    files: Vec<Arc<LogFileIndex>>,
    /// cumulative[i] = 前 i 个文件的行数之和，长度 = files.len() + 1。
    cumulative: Vec<usize>,
}

impl LogFileIndex {
    /// 打开 `path` 并建立行偏移索引。
    ///
    /// 索引不改变文件内容：行尾的 `\n` 与 `\r\n` 会在 [`Self::line`] 读取时剥离。
    /// 空文件返回 [`IndexError::Empty`]，因为 `Mmap::map` 对 0 长度文件会 panic。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexError> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).map_err(|source| IndexError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let len = file
            .metadata()
            .map_err(|source| IndexError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .len();

        if len == 0 {
            return Err(IndexError::Empty(path.to_path_buf()));
        }
        if len > MAX_FILE_BYTES {
            return Err(IndexError::TooLarge {
                bytes: len,
                limit: MAX_FILE_BYTES,
            });
        }

        // SAFETY: `file` 以只读方式打开；在本函数返回后调用方不得通过其它句柄写入或
        // truncate 该文件（见 spec §13 已知限制：macOS 上文件被外部 truncate 会 SIGBUS）。
        // `len > 0` 已在上文保证，避免 memmap2 对空映射 panic。
        let mmap = unsafe { Mmap::map(&file) }.map_err(|source| IndexError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let content_start = if mmap.starts_with(BOM) { BOM.len() } else { 0 };
        let body = &mmap[content_start..];

        // 预估平均行长 80 B，减少首次扩容；offsets 存的是「相对 mmap 的绝对偏移」。
        let mut line_offsets = Vec::with_capacity(body.len() / 80 + 1);
        if !body.is_empty() {
            line_offsets.push(content_start); // 第一行从内容起点开始
        }
        for (i, &b) in body.iter().enumerate() {
            if b == b'\n' && i + 1 < body.len() {
                // 非末尾的 `\n` 之后即为下一行起点（绝对偏移 = content_start + i + 1）
                line_offsets.push(content_start + i + 1);
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            mmap: Arc::new(mmap),
            line_offsets,
        })
    }

    /// 总行数（空文件为 0，对应 D10；空文件不会进入本结构）。
    #[inline]
    pub fn line_count(&self) -> usize {
        self.line_offsets.len()
    }

    /// mmap 的字节长度（含 BOM，用于状态栏总体积统计）。
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.mmap.len()
    }

    /// 取第 `line_idx` 行（0-based），越界返回 `None`。
    ///
    /// 行内容不包含行尾的 `\n` / `\r\n`。当该行包含无效 UTF-8 时返回
    /// `Cow::Owned`，其中无效字节被替换为 U+FFFD（不丢行，对应 G3）。
    pub fn line(&self, line_idx: usize) -> Option<Cow<'_, str>> {
        let bytes = self.line_bytes(line_idx)?;
        Some(String::from_utf8_lossy(bytes))
    }

    /// 原始字节切片（导出用，避免 UTF-8 往返损失）。已剥离行尾 `\r\n` / `\n`。
    pub fn line_bytes(&self, line_idx: usize) -> Option<&[u8]> {
        let start = *self.line_offsets.get(line_idx)?;
        let end = match self.line_offsets.get(line_idx + 1) {
            Some(&next) => next,     // 下一行起点，正好跨过本行 `\n`
            None => self.mmap.len(), // 最后一行，到文件末尾
        };
        let end = strip_eol(&self.mmap, end, start);
        Some(&self.mmap[start..end])
    }

    /// 索引占用的堆内存（字节）：每个偏移 8 字节（spec §11 验收）。
    #[inline]
    pub fn index_memory_bytes(&self) -> usize {
        self.line_offsets.len() * 8
    }

    /// 生成行索引分块 `[lo, hi)`，每块字节量约 `target_bytes`，块边界对齐到行起点。
    ///
    /// 返回的是「行号区间」而非字节区间：每个区间内的行可被独立并行扫描，
    /// 因为块边界恰好落在某行的 `line_offsets` 起点上，不会切断行（修复 D5）。
    pub fn chunk_bounds(&self, target_bytes: usize) -> Vec<(usize, usize)> {
        let n = self.line_count();
        if n == 0 {
            return Vec::new();
        }
        let target = target_bytes.max(1);
        let mut out = Vec::new();
        let mut lo = 0usize;
        let mut acc = 0usize;
        for i in 1..=n {
            let end = if i < n {
                self.line_offsets[i]
            } else {
                self.mmap.len()
            };
            acc += end - self.line_offsets[lo];
            if acc >= target || i == n {
                out.push((lo, i));
                lo = i;
                acc = 0;
            }
        }
        if lo < n {
            out.push((lo, n));
        }
        out
    }

    /// 第 `line_idx` 行在 mmap 中的起始字节偏移（绝对偏移，已含 BOM 跳过）。
    // M5 导出流式写行时按字节偏移直读 mmap，本里程碑 UI 暂不消费。
    #[allow(dead_code)]
    pub fn line_start(&self, line_idx: usize) -> Option<usize> {
        self.line_offsets.get(line_idx).copied()
    }

    /// 行区间 `[lo, hi)` 覆盖的字节长度（含最后一行到其行尾的内容）。
    pub fn byte_span(&self, lo: usize, hi: usize) -> u64 {
        let start = self.line_offsets.get(lo).copied().unwrap_or(0);
        let end = if hi < self.line_count() {
            self.line_offsets
                .get(hi)
                .copied()
                .unwrap_or(self.mmap.len())
        } else {
            self.mmap.len()
        };
        (end - start) as u64
    }
}

impl FileSet {
    /// 新建空集合（cumulative 预置一个 0 哨兵）。
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            cumulative: vec![0],
        }
    }

    /// 追加一个已索引文件（调用方应已处理 [`IndexError::Empty`]）。
    pub fn push(&mut self, file: Arc<LogFileIndex>) {
        let next = self.cumulative.last().copied().unwrap_or(0) + file.line_count();
        self.cumulative.push(next);
        self.files.push(file);
    }

    /// 已加载文件个数。
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 按索引取文件（只读引用），越界返回 `None`。
    pub fn file(&self, idx: usize) -> Option<&Arc<LogFileIndex>> {
        self.files.get(idx)
    }

    /// 已加载文件列表（只读），供检索引擎快照扫描（spec §7.3 / M4）。
    pub fn files(&self) -> &[Arc<LogFileIndex>] {
        &self.files
    }

    /// 全局总行数。
    #[inline]
    pub fn total_lines(&self) -> usize {
        *self.cumulative.last().unwrap_or(&0)
    }

    /// 全局字节总量（含 BOM）。
    pub fn total_bytes(&self) -> usize {
        self.files.iter().map(|f| f.byte_len()).sum()
    }

    /// 全局行号 → `(file_idx, line_idx)`；越界返回 `None`。
    pub fn resolve(&self, global_idx: usize) -> Option<(usize, usize)> {
        if global_idx >= self.total_lines() {
            return None;
        }
        let file_idx = match self.cumulative.binary_search(&global_idx) {
            // 恰好落在某文件第 0 行起点
            Ok(j) => j,
            // 落在 [cumulative[i-1], cumulative[i]) 内
            Err(i) => i.saturating_sub(1),
        };
        if file_idx >= self.files.len() {
            return None;
        }
        let line_idx = global_idx - self.cumulative[file_idx];
        Some((file_idx, line_idx))
    }

    /// 全局行号 → 该行文本（lossy）。越界返回 `None`。
    pub fn line(&self, global_idx: usize) -> Option<Cow<'_, str>> {
        let (f, l) = self.resolve(global_idx)?;
        self.files[f].line(l)
    }
}

impl Default for FileSet {
    fn default() -> Self {
        Self::new()
    }
}

/// 从行尾剥掉一个行结束符（`\n`，以及其前的 `\r`），返回新的结束偏移。
#[inline]
fn strip_eol(mmap: &[u8], mut end: usize, start: usize) -> usize {
    if end > start {
        if mmap[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && mmap[end - 1] == b'\r' {
            end -= 1;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn open_empty_returns_empty_error() {
        let err = LogFileIndex::open(fixture("empty.log")).unwrap_err();
        assert!(matches!(err, IndexError::Empty(_)));
    }

    #[test]
    fn open_missing_file_returns_io_error() {
        let err = LogFileIndex::open(fixture("does_not_exist.log")).unwrap_err();
        assert!(matches!(err, IndexError::Io { .. }));
    }

    #[test]
    fn no_trailing_newline_counts_three_lines() {
        let idx = LogFileIndex::open(fixture("no_trailing_newline.log")).unwrap();
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line(0).unwrap(), "first line");
        assert_eq!(idx.line(1).unwrap(), "second line");
        // 末行不以 \n 结尾仍算一行，且不带换行符
        assert_eq!(idx.line(2).unwrap(), "third line without newline");
    }

    #[test]
    fn crlf_is_stripped() {
        let idx = LogFileIndex::open(fixture("crlf.log")).unwrap();
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line(0).unwrap(), "crlf line one");
        assert_eq!(idx.line(1).unwrap(), "crlf line two");
        assert_eq!(idx.line(2).unwrap(), "crlf line three");
    }

    #[test]
    fn bom_is_skipped() {
        let idx = LogFileIndex::open(fixture("bom.log")).unwrap();
        assert_eq!(idx.line_count(), 2);
        // 首部 BOM 被跳过，不出现在首行内容里
        assert_eq!(idx.line(0).unwrap(), "bom first line");
        assert_eq!(idx.line(1).unwrap(), "bom second line");
    }

    #[test]
    fn invalid_utf8_is_lossy_not_dropped() {
        let idx = LogFileIndex::open(fixture("invalid_utf8.log")).unwrap();
        // 3 行，含无效字节的行不丢，被 U+FFFD 替代（G3）。
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line(0).unwrap(), "valid line before");
        assert!(idx.line(1).unwrap().contains('\u{FFFD}'));
        assert_eq!(idx.line(2).unwrap(), "valid line after");
    }

    #[test]
    fn two_empty_lines() {
        let dir = std::env::temp_dir().join("hyperlog_two_blank.log");
        std::fs::write(&dir, b"\n\n").unwrap();
        let idx = LogFileIndex::open(&dir).unwrap();
        assert_eq!(idx.line_count(), 2);
        assert_eq!(idx.line(0).unwrap(), "");
        assert_eq!(idx.line(1).unwrap(), "");
    }

    #[test]
    fn multi_file_global_addressing() {
        let mut set = FileSet::new();
        for name in ["a.log", "b.log", "c.log"] {
            let idx = Arc::new(LogFileIndex::open(fixture("multi").join(name)).unwrap());
            set.push(idx);
        }
        // a=5, b=3, c=7 → total 15
        assert_eq!(set.total_lines(), 15);
        assert_eq!(set.resolve(0), Some((0, 0)));
        assert_eq!(set.resolve(4), Some((0, 4)));
        assert_eq!(set.resolve(5), Some((1, 0)));
        assert_eq!(set.resolve(7), Some((1, 2)));
        assert_eq!(set.resolve(8), Some((2, 0)));
        assert_eq!(set.resolve(14), Some((2, 6)));
        assert_eq!(set.resolve(15), None);
        // 跨文件取行内容与单文件一致
        assert_eq!(set.line(5).unwrap(), "B line 1");
        assert_eq!(set.line(14).unwrap(), "C line 7");
    }

    #[test]
    fn index_memory_is_eight_bytes_per_line() {
        let idx = LogFileIndex::open(fixture("crlf.log")).unwrap();
        assert_eq!(idx.index_memory_bytes(), idx.line_count() * 8);
    }
}
