#!/usr/bin/env python3
"""cache-usage.jsonl 取证报告：命中率到底被什么吃掉了。

用法：
    python3 testkit/cache-forensics/report.py [--days 3] [--session SID] [--logs DIR]

日志只有 prompt / cache_read 两个数的时候，「我们把前缀掰了」和「上游丢了
缓存」长得一模一样。加了 `sess`/`turn`/`same`/`prev` 之后才分得开，这个脚本
就是把那几列读成结论：

- **供应商不报缓存**：`reported=false`（antigravity 就是）—— `cache_read` 恒 0
  是「没报数」不是「没命中」，可它照样进 Σ 的分母，会把整个会话的百分比
  拖下水（09-22 实测：87.3% 的 opencodego 和不报数的 agy 一起算，Σ 只剩
  42.6%）。所以单列。
- **冷启动**：会话第一条请求，没有上一次可比 —— 必然 miss，不算问题。
- **换进程**：进程内的 `prev` 跨不过重启，但 `sys`/`tools_hash` 两个指纹
  跨得过（09-24 起）。拿同会话日志里的上一行比：两个都没变是「换进程·开头
  没变」（这时再没命中就是上游）；变了是「换进程·提示词或工具面变了」——
  装了新构建、换了模式或功能表。
- **纯追加却全断**：`same >= prev`（上一次的每条都还在、一字未改）却
  `cache_read=0` —— 我们没动前缀，锅在上游（逐出 / 过期 / 打到冷节点）。
- **前缀被改写**：`same < prev`，`at`/`role` 指出第几条被重写了 —— 我们的锅。
- **工具表变了**：`tools_changed` —— 工具面换脸同样掰整段前缀。
- **换了缓存域**：同会话相邻两条请求的 provider/model 不同 —— 换模型等于换
  缓存，那一轮必然全 miss。

没有指纹列的老日志（09-22 及之前）只会走到「无指纹」那一档，那时只能看
总量。
"""

import argparse
import collections
import glob
import json
import os
import sys

def load(logs_dir, days):
    """按日期倒序读最近 `days` 天的记账行。"""
    files = sorted(glob.glob(os.path.join(logs_dir, "cache-usage.*.jsonl")))[-days:]
    rows = []
    for path in files:
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    rows.sort(key=lambda row: row.get("ts", ""))
    return rows, files


def classify(rows):
    """给每条 chat 请求定性，返回 (类别, 行) 列表。

    `sess` 缺失的老行归入「无指纹」；旁路 scope（判官/记忆整理/标题）单列，
    它们的前缀天生短，混进主对话的统计里只会污染结论。
    """
    verdicts = []
    previous_by_session = {}
    for row in rows:
        scope = row.get("scope", "")
        session = row.get("sess")
        prompt = row.get("prompt", 0)
        read = row.get("cache_read", 0)
        if scope != "chat":
            verdicts.append(("旁路 scope", row))
            continue
        if not row.get("reported"):
            # 先于一切判定：供应商压根没报缓存账，后面几档全都无从谈起。
            verdicts.append(("供应商不报缓存", row))
            continue
        if session is None:
            verdicts.append(("无指纹（老日志）", row))
            continue
        previous = previous_by_session.get(session)
        previous_by_session[session] = row
        domain_changed = previous is not None and (
            previous.get("provider") != row.get("provider")
            or previous.get("model") != row.get("model")
        )
        if row.get("prev") is None:
            if previous is None or "sys" not in row or "sys" not in previous:
                verdicts.append(("冷启动（无上一次）", row))
            elif (row["sys"], row.get("tools_hash")) != (
                previous["sys"],
                previous.get("tools_hash"),
            ):
                verdicts.append(("换进程·提示词或工具面变了", row))
            else:
                verdicts.append(("换进程·开头没变", row))
        elif domain_changed:
            verdicts.append(("换了缓存域（模型/供应商变了）", row))
        elif row.get("tools_changed"):
            verdicts.append(("工具表变了", row))
        elif row.get("same", 0) < row.get("prev", 0):
            verdicts.append(("前缀被改写", row))
        elif read == 0 and prompt > 0:
            verdicts.append(("纯追加却全断（上游）", row))
        else:
            verdicts.append(("正常", row))
    return verdicts


def table(title, headers, rows):
    if not rows:
        return
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(str(cell)))
    print(f"\n{title}")
    print("  " + "  ".join(h.ljust(widths[i]) for i, h in enumerate(headers)))
    print("  " + "  ".join("-" * widths[i] for i in range(len(headers))))
    for row in rows:
        print("  " + "  ".join(str(c).ljust(widths[i]) for i, c in enumerate(row)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--days", type=int, default=3, help="读最近几天（默认 3）")
    parser.add_argument("--session", help="只看这一个会话 id")
    parser.add_argument(
        "--logs",
        default=os.path.expanduser("~/.miyu/cache/logs"),
        help="日志目录（默认 ~/.miyu/cache/logs）",
    )
    parser.add_argument("--limit", type=int, default=20, help="逐条列出多少条异常")
    args = parser.parse_args()

    rows, files = load(args.logs, args.days)
    if not rows:
        print(f"{args.logs} 下没读到 cache-usage 日志", file=sys.stderr)
        return 1
    if args.session:
        rows = [row for row in rows if row.get("sess") == args.session]
    print(f"读了 {len(files)} 个文件、{len(rows)} 条记录：")
    for path in files:
        print(f"  {path}")

    verdicts = classify(rows)

    counts = collections.Counter(verdict for verdict, _ in verdicts)
    totals = collections.defaultdict(lambda: [0, 0])
    for verdict, row in verdicts:
        slot = totals[verdict]
        slot[0] += row.get("prompt", 0)
        slot[1] += row.get("cache_read", 0)
    table(
        "定性分布（chat 为主对话，旁路 scope 另计）",
        ["定性", "请求数", "prompt", "cache_read", "命中%"],
        [
            (
                verdict,
                counts[verdict],
                totals[verdict][0],
                totals[verdict][1],
                f"{totals[verdict][1] / totals[verdict][0] * 100:.1f}"
                if totals[verdict][0]
                else "—",
            )
            for verdict in sorted(counts, key=lambda k: -totals[k][0])
        ],
    )

    # 每个会话一行：用户在 footer 上看到的就是这个 Σ 口径。
    per_session = collections.defaultdict(lambda: [0, 0, 0])
    for row in rows:
        if row.get("scope") != "chat":
            continue
        slot = per_session[row.get("sess") or "(无指纹)"]
        slot[0] += row.get("prompt", 0)
        slot[1] += row.get("cache_read", 0)
        slot[2] += 1
    reported = [row for row in rows if row.get("scope") == "chat" and row.get("reported")]
    every = [row for row in rows if row.get("scope") == "chat"]
    def rate(sample):
        prompt = sum(row.get("prompt", 0) for row in sample)
        read = sum(row.get("cache_read", 0) for row in sample)
        return f"{read / prompt * 100:.1f}%" if prompt else "—"
    print(
        f"\n主对话命中率：footer 的 Σ 口径 {rate(every)}"
        f"  /  只算会报缓存的供应商 {rate(reported)}"
    )

    table(
        "按会话（Σ 口径，和 footer 一致）",
        ["会话", "请求数", "prompt", "cache_read", "命中%"],
        [
            (
                session,
                slot[2],
                slot[0],
                slot[1],
                f"{slot[1] / slot[0] * 100:.1f}" if slot[0] else "—",
            )
            for session, slot in sorted(per_session.items(), key=lambda kv: -kv[1][0])
        ],
    )

    blame = [
        (verdict, row)
        for verdict, row in verdicts
        if verdict
        in (
            "前缀被改写",
            "工具表变了",
            "换了缓存域",
            "纯追加却全断（上游）",
            "换进程·提示词或工具面变了",
        )
    ]
    table(
        f"异常逐条（最近 {args.limit} 条，共 {len(blame)} 条）",
        [
            "时间",
            "定性",
            "会话",
            "模型",
            "prompt",
            "read",
            "msgs",
            "prev",
            "same",
            "at",
            "role",
            "工具变动",
        ],
        [
            (
                row.get("ts", "")[11:19],
                verdict,
                (row.get("sess") or "-")[:18],
                (row.get("model") or "-")[-18:],
                row.get("prompt", 0),
                row.get("cache_read", 0),
                row.get("msgs", "-"),
                row.get("prev", "-"),
                row.get("same", "-"),
                row.get("at", "-"),
                row.get("role", "-"),
                (row.get("tools_diff") or "-")[:40],
            )
            for verdict, row in blame[-args.limit :]
        ],
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
