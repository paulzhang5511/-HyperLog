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

/// 千分位分组：`12345678` → `"12,345,678"`（状态栏的行数显示）。
pub fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        // 从右往左每三位插一个逗号：剩余位数 = len - i，是 3 的倍数且不是首位时插分隔符
        let remaining = bytes.len() - i;
        if i > 0 && remaining.is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// 导出默认文件名用的本地日历时间戳：`YYYYMMDD-HHMMSS`（spec T17）。
pub fn export_filename_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_uses_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MiB");
    }

    #[test]
    fn group_digits_inserts_separators() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(12_345_678), "12,345,678");
        assert_eq!(group_digits(100), "100");
    }
}
