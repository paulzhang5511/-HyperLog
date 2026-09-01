#!/usr/bin/env bash
#
# 生成合成日志样本，用于性能验收（spec.md §11.2）。
#
# 用法: scripts/gen_log.sh <输出路径> <行数>
#   scripts/gen_log.sh /tmp/bench_1gb.log 10000000   # ≈ 1 GB（平均行长 ~100 B）
#
# 输出为确定性内容（固定随机种子），同一参数多次生成结果一致。
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "用法: $0 <输出路径> <行数>" >&2
    exit 1
fi

DEST="$1"
LINES="$2"

if ! [[ "$LINES" =~ ^[0-9]+$ ]] || [ "$LINES" -lt 1 ]; then
    echo "错误: 行数必须是正整数，收到 '$LINES'" >&2
    exit 1
fi

mkdir -p "$(dirname "$DEST")"

awk -v total="$LINES" '
BEGIN {
    srand(20260901);

    split("DEBUG INFO WARN ERROR", levels, " ");
    split("TRACE DEBUG INFO WARN ERROR FATAL", all_levels, " ");
    split("http-nio-8080-exec-1 http-nio-8080-exec-2 scheduler-1 pool-3-thread-1", threads, " ");
    split("User Session Cache Request Token Payment Order Invoice", subjects, " ");
    split("initialized validated refreshed completed rejected expired", verbs, " ");

    # 固定时间戳前缀（避免依赖平台相关的 strftime；perf 样本不要求逐行时间真实）
    ts_prefix = "2026-06-01 12:00:00";

    for (i = 0; i < total; i++) {
        # 约 5% 的行是 ERROR，便于检索基准测试
        lvl = (i % 20 == 0) ? "ERROR" : levels[int(rand() * 4) + 1];

        th = threads[int(rand() * 4) + 1];
        subject = subjects[int(rand() * 8) + 1];
        verb = verbs[int(rand() * 6) + 1];

        printf "%s.%03d  %-5s [%s] %s %s successfully in %d ms (id=%08d)\n",
            ts_prefix, i % 1000, lvl, th, subject, verb,
            int(rand() * 900) + 1, i;
    }
}
' > "$DEST"

size=$(du -h "$DEST" | cut -f1)
echo "已生成 ${DEST}：${LINES} 行，${size}"
