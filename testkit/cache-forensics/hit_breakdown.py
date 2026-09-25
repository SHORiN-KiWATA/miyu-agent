#!/usr/bin/env python3
"""缓存命中分解(09-25):把 cache-usage.jsonl 逐请求拆成「首请求 / 跨轮 / 轮内」,按会话角色分组,
每条没打满的请求归因到我方还是供应商。

两个数:
    命中率      cache_read / prompt                 这条请求里有多少是缓存读的
    前缀覆盖率  min(cache_read, 已发过) / 已发过     上一条请求已经发过的那段,这次有多少吃到了缓存
                (已发过 = min(上一条的 prompt, 这一条的 prompt));新追加的内容天然不在缓存里,
                不算进覆盖率的分母

没盖住的「已发过」token 按下面的次序归因(先中先得):
    switch          换了供应商或模型,缓存本来就不通
    granularity     缺口不超过 GRANULE-1(这条线路按 128 token 一块存,末尾不满一块的零头)
    ours:rewrite    前缀指纹 same < prev:上一条请求里有消息被改写/删掉了(at/role 指出第几条)
    ours:sys        系统提示词的散列变了
    ours:tools      工具表的散列变了
    gap             上一条日志行和这一条之间还有一条没落日志的请求(打断时在飞的那条),比的对象不准
    theirs          纯追加、sys 与工具表都没变,缓存却没盖住:供应商那边的事

分组:
    first   这个会话在窗口里的第一条请求
    cross   换了 turn 之后的第一条请求
    within  同一 turn 的后续请求

用法:
    hit_breakdown.py <cache-usage.jsonl>... [--sess ID[=角色]]... [--since HH:MM[:SS]] [--until HH:MM[:SS]]
                     [--scope chat] [--provider opencodego] [--rows] [--json OUT]
    不给 --sess 就按 sess 字段自动分组(sess 为空的按 scope 归到「辅助」)。
    角色是给表格分组用的标签,例如 main / sub:调研A / aux。
"""

import argparse
import json
import sys
from collections import OrderedDict, defaultdict

GRANULE = 128
CAUSES = ("switch", "granularity", "ours:rewrite", "ours:sys", "ours:tools", "gap", "theirs")


def load(paths):
    rows = []
    for path in paths:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    rows.append(json.loads(line))
                except ValueError:
                    continue
    rows.sort(key=lambda row: row.get("ts") or "")
    return rows


def clock(row):
    """ts 形如 2026-09-25T20:21:38.29+09:00;取本地时分秒比较。"""
    ts = row.get("ts") or ""
    return ts[11:19]


def chain_key(row):
    """前缀链按 (scope, sess) 各算各的;没有会话的辅助请求按 scope 归一条链。"""
    return (row.get("scope") or "", row.get("sess") or "")


def analyze_chain(rows):
    out = []
    previous = None
    for row in rows:
        prompt = row.get("prompt") or 0
        read = row.get("cache_read") or 0
        if previous is None:
            kind = "first"
        elif row.get("turn") and row.get("turn") == previous.get("turn"):
            kind = "within"
        else:
            kind = "cross"
        record = {
            "ts": clock(row),
            "turn": row.get("turn"),
            "kind": kind,
            "prompt": prompt,
            "read": read,
            "msgs": row.get("msgs"),
            "prev": row.get("prev"),
            "same": row.get("same"),
            "at": row.get("at"),
            "role": row.get("role"),
            "provider": row.get("provider"),
            "model": row.get("model"),
            "resent": 0,
            "missed": 0,
            "cause": None,
        }
        if previous is not None:
            switched = (row.get("provider"), row.get("model")) != (previous.get("provider"), previous.get("model"))
            resent = min(previous.get("prompt") or 0, prompt)
            missed = max(0, resent - read)
            record["resent"] = resent
            record["missed"] = missed
            if switched:
                cause = "switch"
            elif missed == 0:
                cause = None
            elif missed < GRANULE:
                cause = "granularity"
            elif row.get("prev") is not None and row.get("same") is not None and row["same"] < row["prev"]:
                cause = "ours:rewrite"
            elif row.get("sys") and previous.get("sys") and row["sys"] != previous["sys"]:
                cause = "ours:sys"
            elif row.get("tools_hash") and previous.get("tools_hash") and row["tools_hash"] != previous["tools_hash"]:
                cause = "ours:tools"
            elif row.get("prev") is not None and previous.get("msgs") is not None and row["prev"] != previous["msgs"]:
                cause = "gap"
            else:
                cause = "theirs"
            record["cause"] = cause
        out.append(record)
        previous = row
    return out


def empty_bucket():
    return {"n": 0, "prompt": 0, "read": 0, "resent": 0, "covered": 0, "missed": defaultdict(int)}


def add(bucket, record):
    bucket["n"] += 1
    bucket["prompt"] += record["prompt"]
    bucket["read"] += record["read"]
    if record["kind"] != "first":
        bucket["resent"] += record["resent"]
        bucket["covered"] += min(record["read"], record["resent"])
        if record["cause"]:
            bucket["missed"][record["cause"]] += record["missed"]


def pct(numerator, denominator):
    return f"{100 * numerator / denominator:6.2f}" if denominator else "     -"


def summarize(analyzed, roles):
    """analyzed: {chain_key: [records]};roles: {sess: 角色}。返回 {(角色, kind): bucket}。"""
    table = OrderedDict()
    for key, records in analyzed.items():
        scope, sess = key
        role = roles.get(sess) or (f"aux:{scope}" if not sess else "other")
        for record in records:
            for bucket_key in ((role, record["kind"]), (role, "all"), ("ALL", record["kind"]), ("ALL", "all")):
                table.setdefault(bucket_key, empty_bucket())
                add(table[bucket_key], record)
    return table


def print_table(table):
    print(f"{'角色':<22} {'分组':<7} {'请求':>4} {'prompt':>10} {'cache_read':>10} {'命中率%':>7} "
          f"{'已发过':>10} {'覆盖率%':>7}  没盖住的已发过 token(按原因)")
    for (role, kind), bucket in table.items():
        missed = ", ".join(f"{cause}={bucket['missed'][cause]}" for cause in CAUSES if bucket["missed"].get(cause))
        print(f"{role:<22} {kind:<7} {bucket['n']:>4} {bucket['prompt']:>10} {bucket['read']:>10} "
              f"{pct(bucket['read'], bucket['prompt']):>7} {bucket['resent']:>10} "
              f"{pct(bucket['covered'], bucket['resent']):>7}  {missed}")


def print_rows(analyzed, roles):
    for key, records in analyzed.items():
        scope, sess = key
        role = roles.get(sess) or (f"aux:{scope}" if not sess else "other")
        print(f"\n[{role}] {sess or scope}")
        print(f"  {'ts':<8} {'kind':<6} {'msgs':>4} {'prev→same':>9} {'prompt':>8} {'read':>8} {'hit%':>6} "
              f"{'resent':>8} {'missed':>7} cause")
        for record in records:
            prev_same = f"{record['prev'] if record['prev'] is not None else '-'}→" \
                        f"{record['same'] if record['same'] is not None else '-'}"
            hit = pct(record["read"], record["prompt"])
            extra = f" at={record['at']} role={record['role']}" if record["cause"] == "ours:rewrite" else ""
            print(f"  {record['ts']:<8} {record['kind']:<6} {record['msgs'] or '-':>4} {prev_same:>9} "
                  f"{record['prompt']:>8} {record['read']:>8} {hit:>6} {record['resent']:>8} "
                  f"{record['missed']:>7} {record['cause'] or ''}{extra}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("files", nargs="+")
    parser.add_argument("--sess", action="append", default=[], help="会话 id,可写成 ID=角色")
    parser.add_argument("--since")
    parser.add_argument("--until")
    parser.add_argument("--scope", action="append", default=[], help="只看这些 scope(可重复)")
    parser.add_argument("--provider", action="append", default=[], help="只看这些供应商(可重复)")
    parser.add_argument("--need-sess", action="store_true",
                        help="丢掉没有会话 id 的行(09-23 之前的老日志行没有 sess,串不成链)")
    parser.add_argument("--rows", action="store_true", help="逐请求明细")
    parser.add_argument("--json", help="把分组结果与逐请求明细写进这个文件")
    args = parser.parse_args(argv)

    roles = {}
    for item in args.sess:
        sess, _, role = item.partition("=")
        roles[sess] = role or sess[:18]
    rows = load(args.files)
    selected = []
    for row in rows:
        at = clock(row)
        if args.since and at < args.since:
            continue
        if args.until and at > args.until:
            continue
        if args.scope and (row.get("scope") or "") not in args.scope:
            continue
        if args.need_sess and not row.get("sess"):
            continue
        if args.provider and (row.get("provider") or "") not in args.provider:
            continue
        if roles and (row.get("sess") or "") not in roles:
            continue
        selected.append(row)
    chains = OrderedDict()
    for row in selected:
        chains.setdefault(chain_key(row), []).append(row)
    analyzed = OrderedDict((key, analyze_chain(chain)) for key, chain in chains.items())
    table = summarize(analyzed, roles)
    print_table(table)
    if args.rows:
        print_rows(analyzed, roles)
    if args.json:
        payload = {
            "table": [
                {"role": role, "kind": kind, **{k: v for k, v in bucket.items() if k != "missed"},
                 "missed": dict(bucket["missed"])}
                for (role, kind), bucket in table.items()
            ],
            "chains": [{"scope": key[0], "sess": key[1], "role": roles.get(key[1]), "requests": records}
                       for key, records in analyzed.items()],
        }
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
