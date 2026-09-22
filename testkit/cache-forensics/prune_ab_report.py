#!/usr/bin/env python3
"""把 A/B 两组沙盒的 cache-usage 读成一张对照表。

A 组开着工具输出剪枝（`tool_result_prune_chars=8192`），B 组关掉。两组跑同一
串会产生大工具输出的提示词，差值就是剪枝的缓存代价。

带指纹的二进制还会给出 `same`/`prev`：A 组若真是被剪枝掰断，它的「前缀被
改写」条数应显著高于 B 组，且 `at` 落在工具结果那条消息上。
"""

import collections
import glob
import json
import os
import sys


def load(logs_dir):
    rows = []
    for path in sorted(glob.glob(os.path.join(logs_dir, "cache-usage.*.jsonl"))):
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
    rows.sort(key=lambda row: row.get("ts", ""))
    return rows


def summarize(name, rows):
    prompt = sum(row.get("prompt", 0) for row in rows)
    read = sum(row.get("cache_read", 0) for row in rows)
    rewritten = [
        row
        for row in rows
        if row.get("prev") is not None and row.get("same", 0) < row.get("prev", 0)
    ]
    full_miss = [
        row
        for row in rows
        if row.get("prev") is not None
        and row.get("cache_read", 0) == 0
        and row.get("prompt", 0) > 0
    ]
    return {
        "组": name,
        "请求": len(rows),
        "prompt": prompt,
        "cache_read": read,
        "命中%": f"{read / prompt * 100:.1f}" if prompt else "—",
        "前缀被改写": len(rewritten),
        "全断": len(full_miss),
        "改写位置": collections.Counter(row.get("role") for row in rewritten).most_common(3),
    }


def main():
    if len(sys.argv) < 3:
        print("用法: prune_ab_report.py <A 日志目录> <B 日志目录>", file=sys.stderr)
        return 2
    arms = [("A prune=8192", load(sys.argv[1])), ("B prune=0", load(sys.argv[2]))]
    rows = [summarize(name, sample) for name, sample in arms]
    if not any(row["请求"] for row in rows):
        print("两组都没读到 chat 记录——先跑 run", file=sys.stderr)
        return 1

    headers = ["组", "请求", "prompt", "cache_read", "命中%", "前缀被改写", "全断"]
    widths = [
        max(len(h), max(len(str(row[h])) for row in rows)) for h in headers
    ]
    print()
    print("  " + "  ".join(h.ljust(widths[i]) for i, h in enumerate(headers)))
    print("  " + "  ".join("-" * widths[i] for i in range(len(headers))))
    for row in rows:
        print("  " + "  ".join(str(row[h]).ljust(widths[i]) for i, h in enumerate(headers)))
    for row in rows:
        if row["改写位置"]:
            print(f"\n  {row['组']} 的改写落点：{row['改写位置']}")

    a, b = rows
    if a["请求"] and b["请求"] and a["命中%"] != "—" and b["命中%"] != "—":
        delta = float(b["命中%"]) - float(a["命中%"])
        print(f"\n  B − A = {delta:+.1f} 个百分点", end="")
        print("（正值即剪枝确实在吃命中率）" if delta > 0 else "（剪枝不是主因）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
