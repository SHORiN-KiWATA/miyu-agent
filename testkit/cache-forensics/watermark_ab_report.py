#!/usr/bin/env python3
"""水位改动前后的对照表。

A 组是改之前（压缩借用裁剪的水位，两者打平），B 组是改之后（压缩有自己更低
的水位，裁剪退为兜底）。四个口径：

- **压缩触发**：日志里 `context_rewrite reason=compact` 的条数。A 组预期为 0
  ——裁剪在回合开头就把上下文压到线下了，压缩等不到自己的条件。
- **裁剪触发**：日志里「上下文裁剪已触发」的条数，以及它一共计划逐出多少轮。
- **轮的去留**：`turns` 表里 seq 的缺口就是被删掉的轮数（裁剪是 DELETE，
  不是折叠）。压缩不删轮，它写摘要行。
- **缓存命中**：`cache-usage.jsonl` 的 Σ 口径，以及前缀被改写的次数。
"""

import glob
import json
import os
import re
import sqlite3
import sys
import tempfile


def read_cache_usage(home):
    rows = []
    for path in sorted(glob.glob(os.path.join(home, "cache/logs/cache-usage.*.jsonl"))):
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if row.get("scope") == "chat":
                    rows.append(row)
    return rows


def count_log(home, needle):
    total = 0
    for path in glob.glob(os.path.join(home, "cache/logs/miyu.*.log")):
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if needle in line:
                    total += 1
    return total


def planned_evictions(home):
    total = 0
    for path in glob.glob(os.path.join(home, "cache/logs/miyu.*.log")):
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if "上下文裁剪已触发" not in line:
                    continue
                match = re.search(r"planned=(\d+)", line)
                if match:
                    total += int(match.group(1))
    return total


def turn_stats(home):
    """seq 的缺口 = 被删掉的轮；`is_summary` 行 = 压缩写下的摘要。"""
    found = glob.glob(os.path.join(home, "home/*/conversation.db"))
    if not found:
        return None
    with tempfile.TemporaryDirectory() as tmp:
        copy = os.path.join(tmp, "c.db")
        for suffix in ("", "-wal", "-shm"):
            try:
                with open(found[0] + suffix, "rb") as src, open(copy + suffix, "wb") as dst:
                    dst.write(src.read())
            except FileNotFoundError:
                pass
        conn = sqlite3.connect(copy)
        try:
            rows, lo, hi, summaries = conn.execute(
                "SELECT COUNT(*), MIN(seq), MAX(seq), COALESCE(SUM(is_summary), 0) FROM turns"
            ).fetchone()
        except sqlite3.OperationalError:
            # daemon 起了但一轮都没跑完时库里还没有 turns 表——报空，别让
            # 整张对照表跟着崩掉。
            return None
        finally:
            conn.close()
    if not rows:
        return None
    return {
        "rows": rows,
        "span": (hi - lo + 1) if lo is not None else 0,
        "missing": ((hi - lo + 1) - rows) if lo is not None else 0,
        "summaries": summaries,
    }


def summarize(name, home):
    rows = read_cache_usage(home)
    prompt = sum(row.get("prompt", 0) for row in rows)
    read = sum(row.get("cache_read", 0) for row in rows)
    rewritten = sum(
        1
        for row in rows
        if row.get("prev") is not None and row.get("same", 0) < row.get("prev", 0)
    )
    turns = turn_stats(home) or {"rows": 0, "missing": 0, "summaries": 0}
    return {
        "组": name,
        "压缩触发": count_log(home, "context_rewrite reason=compact"),
        "裁剪触发": count_log(home, "上下文裁剪已触发"),
        "计划逐出轮": planned_evictions(home),
        "现存轮": turns["rows"],
        "被删轮": turns["missing"],
        "摘要行": turns["summaries"],
        "prompt": prompt,
        "cache_read": read,
        "命中%": f"{read / prompt * 100:.1f}" if prompt else "—",
        "前缀被改写": rewritten,
    }


def main():
    if len(sys.argv) < 3:
        print("用法: watermark_ab_report.py <A 家目录> <B 家目录>", file=sys.stderr)
        return 2
    rows = [summarize("A 改之前", sys.argv[1]), summarize("B 改之后", sys.argv[2])]
    headers = [
        "组",
        "压缩触发",
        "裁剪触发",
        "计划逐出轮",
        "现存轮",
        "被删轮",
        "摘要行",
        "prompt",
        "cache_read",
        "命中%",
        "前缀被改写",
    ]
    widths = [max(len(h), max(len(str(row[h])) for row in rows)) for h in headers]
    print()
    print("  " + "  ".join(h.ljust(widths[i]) for i, h in enumerate(headers)))
    print("  " + "  ".join("-" * widths[i] for i in range(len(headers))))
    for row in rows:
        print("  " + "  ".join(str(row[h]).ljust(widths[i]) for i, h in enumerate(headers)))
    a, b = rows
    print()
    print(f"  压缩触发  A {a['压缩触发']} → B {b['压缩触发']}")
    print(f"  裁剪删轮  A {a['被删轮']} → B {b['被删轮']}")
    if a["命中%"] != "—" and b["命中%"] != "—":
        print(f"  缓存命中  A {a['命中%']}% → B {b['命中%']}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
