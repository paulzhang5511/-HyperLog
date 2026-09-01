#!/usr/bin/env bash
#
# 重新生成 tests/fixtures/ 下的全部边界样本（spec.md §9.3）。
# 样本本身入库，仅在本脚本内容变更时才需要重跑。
#
# 用法: scripts/gen_fixtures.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixtures="$root/tests/fixtures"

rm -rf "$fixtures"
mkdir -p "$fixtures/multi"

# 1. 空文件（0 字节）—— 验证 mmap 不会 panic，行数为 0
: > "$fixtures/empty.log"

# 2. 无尾换行 —— 末行不以 \n 结尾仍应算作一行
printf 'first line\nsecond line\nthird line without newline' > "$fixtures/no_trailing_newline.log"

# 3. CRLF 换行 —— 行尾 \r 必须被剥离
printf 'crlf line one\r\ncrlf line two\r\ncrlf line three\r\n' > "$fixtures/crlf.log"

# 4. UTF-8 BOM —— 首部 BOM 必须跳过，不出现在首行内容里
printf '\xEF\xBB\xBFbom first line\nbom second line\n' > "$fixtures/bom.log"

# 5. 含无效 UTF-8 字节 —— 必须 lossy 替换，不得丢行
{
    printf 'valid line before\n'
    printf 'broken \xFF\xFE bytes here\n'
    printf 'valid line after\n'
} > "$fixtures/invalid_utf8.log"

# 6. 日志级别与干扰串 —— errors=3 / INFORMATION / TERROR 不得被识别为级别
cat > "$fixtures/levels.log" <<'EOF'
2026-09-01 10:00:00.001 DEBUG entering request handler
2026-09-01 10:00:00.002 INFO  user login succeeded
2026-09-01 10:00:00.003 WARN  cache miss ratio high
2026-09-01 10:00:00.004 ERROR database connection refused
2026-09-01 10:00:00.005 INFO  retry config: errors=3 max attempts
2026-09-01 10:00:00.006 INFO  INFORMATION schema queried
2026-09-01 10:00:00.007 INFO  TERROR is not a level
2026-09-01 10:00:00.008 FATAL out of memory
2026-09-01 10:00:00.009 TRACE exiting request handler
EOF

# 7. 超长单行（1 MB）—— 必须被渲染截断而非卡死
head -c 1048576 /dev/zero | tr '\0' 'x' > "$fixtures/long_line.log"
printf '\n' >> "$fixtures/long_line.log"

# 8. 多文件样本 —— 验证 FileSet 全局行寻址（D1）
printf 'A line 1\nA line 2\nA line 3\nA line 4\nA line 5\n' > "$fixtures/multi/a.log"
printf 'B line 1\nB line 2\nB line 3\n' > "$fixtures/multi/b.log"
printf 'C line 1\nC line 2\nC line 3\nC line 4\nC line 5\nC line 6\nC line 7\n' > "$fixtures/multi/c.log"

echo "fixtures 已生成于 $fixtures"
ls -la "$fixtures" "$fixtures/multi"
