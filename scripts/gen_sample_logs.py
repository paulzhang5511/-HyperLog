#!/usr/bin/env python3
"""生成 5 个不同规模、不同内容的测试日志文件（本地调试用，不入库）。

用法:
    python3 scripts/gen_sample_logs.py [输出目录]

默认输出到 ./sample_logs/，生成：
  1. tiny.log        小型（~1 KB，几十行）—— 手工肉眼核对渲染/着色
  2. small.log       小型（~100 KB）—— 包含中文、异常堆栈、JSON、时间戳多种形态
  3. medium.log      中型（~10 MB）—— Web 服务器访问日志
  4. large.log       大型（~100 MB）—— 通用应用日志，混合级别/线程/堆栈
  5. xlarge.log      超大型（~1 GB）—— 性能压测样本（对齐 spec §11.2 的 bench 样本）

内容确定性：固定随机种子，同一参数多次生成结果一致。
"""
import os
import random
import sys

OUT = sys.argv[1] if len(sys.argv) > 1 else "sample_logs"
os.makedirs(OUT, exist_ok=True)

random.seed(20260902)

LEVELS = ["DEBUG", "INFO", "WARN", "ERROR", "FATAL"]
LEVELS_W = ["DEBUG", "INFO", "WARN", "WARNING", "ERROR", "TRACE"]
THREADS = [
    "http-nio-8080-exec-1",
    "http-nio-8080-exec-2",
    "scheduler-1",
    "pool-3-thread-1",
    "main",
    "GC-Monitor",
]
MODULES = [
    "com.example.user.UserService",
    "com.example.order.OrderService",
    "com.example.cache.CacheManager",
    "com.example.payment.PaymentGateway",
    "com.example.api.ApiController",
    "com.example.db.ConnectionPool",
]

# 中文消息片段，验证 CJK 渲染与宽度估算
ZH_MSGS = [
    "用户登录成功，令牌已下发",
    "订单支付超时，已回滚事务",
    "缓存命中率下降，触发预热",
    "数据库连接池扩容至 32",
    "请求参数校验失败，缺少必填字段",
    "定时任务执行完毕，共处理 1024 条记录",
]


def ts(i: int) -> str:
    """确定性时间戳：ISO 形态（带毫秒）。"""
    hh = (i // 3600) % 24
    mm = (i // 60) % 60
    ss = i % 60
    ms = i % 1000
    return f"2026-06-01 {hh:02d}:{mm:02d}:{ss:02d}.{ms:03d}"


def app_line(i: int, with_stack: bool = False, zh: bool = False) -> str:
    lvl = LEVELS[random.randrange(len(LEVELS))]
    th = THREADS[random.randrange(len(THREADS))]
    mod = MODULES[random.randrange(len(MODULES))]
    if zh and random.random() < 0.3:
        msg = random.choice(ZH_MSGS)
    else:
        subject = random.choice(
            ["session", "cache", "request", "token", "payment", "order", "invoice"]
        )
        verb = random.choice(
            ["initialized", "validated", "refreshed", "completed", "rejected", "expired"]
        )
        msg = f"{subject} {verb} successfully in {random.randrange(1, 900)} ms (id={i:08d})"
    line = f"{ts(i)}  {lvl:<5} [{th}] {mod} - {msg}"
    if with_stack and lvl in ("ERROR", "FATAL") and random.random() < 0.6:
        ex = random.choice(["NullPointerException", "TimeoutException", "SQLException"])
        line += (
            f"\n\tat {mod}.handle({random.choice('abcdefghij')}.java:{random.randrange(1, 500)})"
            f"\n\tat com.example.core.Dispatcher.dispatch(Dispatcher.java:87)"
            f"\nCaused by: {ex}: {msg}"
        )
    return line


def access_line(i: int) -> str:
    """Nginx 风格访问日志（Common Log Format 变体）。"""
    ips = ["10.0.0.1", "10.0.0.2", "172.16.3.10", "192.168.1.100", "203.0.113.7"]
    methods = ["GET", "POST", "PUT", "DELETE"]
    paths = [
        "/api/users",
        "/api/orders",
        "/api/payments",
        "/health",
        "/static/app.js",
        "/api/cache/refresh",
    ]
    codes = [200, 200, 200, 201, 204, 304, 400, 404, 500, 503]
    ip = random.choice(ips)
    m = random.choice(methods)
    p = random.choice(paths)
    code = random.choice(codes)
    size = random.randrange(100, 50_000) if code < 400 else random.randrange(0, 500)
    lat = random.randrange(1, 5000)
    return (
        f'{ip} - - [{ts(i).replace(" ", "T")}Z] "{m} {p} HTTP/1.1" '
        f"{code} {size} \"-\" \"Mozilla/5.0\" {lat}ms"
    )


def tiny() -> list[str]:
    """小型：几行固定内容，覆盖各类边界（级别、中文、堆栈、时间戳、超长行）。"""
    lines = [
        f"{ts(0)}  INFO  [main] com.example.App - 应用启动完成",
        f"{ts(1)}  DEBUG [main] com.example.db.ConnectionPool - 连接池初始化：size=10",
        f"{ts(2)}  INFO  [http-nio-8080-exec-1] com.example.api.ApiController - 用户登录成功，令牌已下发",
        f"{ts(3)}  WARN  [scheduler-1] com.example.cache.CacheManager - 缓存命中率下降，触发预热",
        f"{ts(4)}  ERROR [http-nio-8080-exec-2] com.example.order.OrderService - 订单支付超时，已回滚事务",
        "\tat com.example.order.OrderService.pay(OrderService.java:143)",
        "\tat com.example.core.Dispatcher.dispatch(Dispatcher.java:87)",
        "\tCaused by: TimeoutException: payment gateway timeout",
        f"{ts(5)}  FATAL [main] com.example.App - 内存溢出，进程即将退出",
        f"{ts(6)}  INFO  [GC-Monitor] com.example.jvm - GC pause 12ms (young 8ms / old 4ms)",
        f"{ts(7)}  TRACE [pool-3-thread-1] com.example.user.UserService - 查询用户详情 id=42",
        # 一条超长行，验证横向滚动与截断
        f"{ts(8)}  INFO  [main] com.example.App - {'x' * 800}\n",
    ]
    return lines


def write(name: str, lines: list[str], expected_note: str):
    path = os.path.join(OUT, name)
    with open(path, "w", encoding="utf-8") as f:
        f.writelines(lines)
    size = os.path.getsize(path)
    print(f"  {name:<16} {len(lines):>12,} 行  {size / 1024 / 1024:>8.2f} MB  ({expected_note})")


def human(n: int) -> str:
    if n >= 1024 * 1024 * 1024:
        return f"{n / 1024 / 1024 / 1024:.1f} GB"
    if n >= 1024 * 1024:
        return f"{n / 1024 / 1024:.1f} MB"
    if n >= 1024:
        return f"{n / 1024:.1f} KB"
    return f"{n} B"


print(f"输出目录: {OUT}/")
print("-" * 72)

# 1. tiny —— 手工核对用（确定性，含中文/堆栈/超长行）
write("tiny.log", tiny(), "小型·手工核对渲染/着色")

# 2. small —— ~100 KB，混合中文/异常堆栈/JSON/时间戳
small_lines = []
for i in range(2_000):
    if i % 7 == 0:
        small_lines.append(
            f'{ts(i)}  INFO  [pool-3-thread-1] com.example.api.ApiController - '
            f'{{"userId": {random.randrange(1, 9999)}, "action": "login", "latencyMs": {random.randrange(1, 500)}}}'
        )
    else:
        small_lines.append(app_line(i, with_stack=True, zh=True) + "\n")
write("small.log", small_lines, "小型·中文/堆栈/JSON")

# 3. medium —— ~10 MB Web 访问日志
medium_lines = [access_line(i) + "\n" for i in range(120_000)]
write("medium.log", medium_lines, "中型·Nginx 访问日志")

# 4. large —— ~100 MB 通用应用日志
large_lines = [app_line(i, with_stack=True) + "\n" for i in range(1_000_000)]
write("large.log", large_lines, "大型·应用日志(级别/线程/堆栈)")

# 5. xlarge —— ~1 GB 性能压测样本（对齐 spec bench）
xlarge_lines = [app_line(i) + "\n" for i in range(10_000_000)]
write("xlarge.log", xlarge_lines, "超大型·性能压测(≈1GB)")

print("-" * 72)
total = sum(os.path.getsize(os.path.join(OUT, n)) for n in os.listdir(OUT))
print(f"合计: {human(total)}（5 个文件，位于 {os.path.abspath(OUT)}/）")
