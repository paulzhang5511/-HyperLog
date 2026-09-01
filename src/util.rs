//! 与 GUI 无关的纯工具函数。

/// 把字节数格式化为人类可读字符串（二进制单位：KiB/MiB/GiB）。
pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// 导出默认文件名用的本地日历时间戳：`YYYYMMDD-HHMMSS`（spec T17）。
pub fn export_filename_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}
