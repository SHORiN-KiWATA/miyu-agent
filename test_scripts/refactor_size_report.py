#!/usr/bin/env python3
"""源文件行数报告与回归门禁。

## 基准是怎么定的

不是拍脑袋，是量出来的。取本机 cargo registry 里 18 个长期维护、被广泛引用
的 crate（tokio / rustls / hyper / axum / reqwest / serde / regex / image /
crossterm / rusqlite / rayon / clap / tracing / url / indexmap / toml /
anyhow / serde_json），统计 1095 个 `.rs` 的行数分布（含内联测试，与本仓库
同口径）：

    中位数 183   P75 427   P90 935   P95 1412   P99 2699   最大 3628

对照 Miyu（126 个文件，181,295 行）：

    中位数 499   P75 1355  P90 3924  P95 8655   最大 16897

于是三条线：

| 线 | 行数 | 依据 |
|---|---|---|
| 目标 | 800 | 优质 crate 里 88% 的文件在此之下（P90=935） |
| 上限 | 1500 | 对齐 P95=1412；越线要在评审里说明理由 |
| 红线 | 2000 | 优质 crate 里只有 2.1% 越过；**禁止新增** |

Rust 官方没有文件行数规范，clippy 的 `too_many_lines` 管的是函数（默认 100
行）。所以这里用实测分布代替主观标准。

## 用法

    python3 scripts/refactor_size_report.py                 # 只报告
    python3 scripts/refactor_size_report.py --write-baseline # 记录基线
    python3 scripts/refactor_size_report.py --check          # 门禁（CI 用）

`--check` 的失败条件（都只针对**恶化**，不要求一次达标）：

1. 出现新的越红线文件；
2. 已越线文件比基线长出 10 行以上（解依赖时拆个 use 就 +1，卡死在 0 会
   把合理的机械改动也拦下来）；
3. 文件总行数比基线增长超过 2%（防止"拆分"变成"复制"）。
"""
import argparse
import json
import re
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src"
SRC_ROOTS = [SRC] + sorted((ROOT / "crates").glob("*/src"))
# 基线跟脚本走,不认目录名(见 arch_dep_check.py 同款修法)。
BASELINE = Path(__file__).resolve().parent / "refactor-size-baseline.json"

TARGET, LIMIT, RED_LINE = 800, 1500, 2000
# 这道门禁防的是「拆分变成了复制代码」，不是防文件变多。每个新模块要带一段
# `//!` 说明自己为什么单独存在，还要有自己的 `use` 和一行 `mod` 声明——291 个
# 新模块下来，光这三样就是 2,758 行（文档 1,339 / 导入 1,001 / 声明 418），
# 占实际增长 3,723 行的 74%。2% 卡在这上面，所以按实测放到 3%。
# 复制代码仍然拦得住：真复制一个模块是几百上千行「其余」，量级完全不同。
# 基线已在拆分合入 main 之后重设（`--write-baseline`），所以这条比的是
# 「相对当前主线」的增长，而不再是「相对拆分前」。日常加功能会让它慢慢逼近
# 上限，届时再重设一次基线即可——它拦的是「一次改动里凭空多出几千行」。
# 上次重设：2026-09-20 发 0.6.1 时。09-17 那份基线（293,598 行）到发版前已经
# 涨到 315,572（+7.5%），涨出来的是 mermaid 渲染、设置界面重画、子代理会话化
# 这批真功能，不是复制代码——逐个文件比过，没有一个越上限或红线。
GROWTH_TOLERANCE = 0.03
# 超标文件允许的小额增长。解依赖时 `use crate::cli::{a, b}` 拆成两行就是 +1，
# 卡死在 0 会把合理的机械改动也拦下来。留一点余量，但仍然拦得住「这个文件
# 又胖了 200 行」。
OVERSIZE_SLACK = 10

CFG_TEST = re.compile(r"^\s*#\[cfg\(test\)\]")


def _strip_noise(line):
    """去掉字符串字面量与行注释里的花括号，避免配对被带偏。"""
    line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
    line = re.sub(r"'(?:[^'\\]|\\.)'", "''", line)
    return line.split("//")[0]


def test_line_count(lines):
    """所有 `#[cfg(test)]` 项占的总行数（花括号配对）。"""
    total, index, count = 0, 0, len(lines)
    while index < count:
        if not CFG_TEST.match(lines[index]):
            index += 1
            continue
        start, depth, seen, cursor = index, 0, False, index
        while cursor < count:
            for char in _strip_noise(lines[cursor]):
                if char == "{":
                    depth += 1
                    seen = True
                elif char == "}":
                    depth -= 1
            if seen and depth <= 0:
                break
            cursor += 1
        total += cursor - start + 1
        index = cursor + 1
    return total


def collect():
    rows = {}
    for path in sorted(p for r in SRC_ROOTS for p in r.rglob("*.rs")):
        lines = path.read_text(encoding="utf-8", errors="replace").split("\n")
        rel = str(path.relative_to(ROOT))
        rows[rel] = {"total": len(lines), "tests": test_line_count(lines)}
    return rows


def percentile(values, q):
    if not values:
        return 0
    ordered = sorted(values)
    return ordered[min(int(len(ordered) * q), len(ordered) - 1)]


# 拆分开始时的欠账：44 个文件超过 800 行目标，超出部分合计 120,108 行。
# 进度按「消化掉多少超标行」算——这个数比「拆了几个文件」诚实，因为把一个
# 16000 行的文件切成两个 8000 行的，文件数变多了但问题一点没解决。
# 拆分开工时的两个数字，只用来显示「从哪儿走到这儿」。拆分已随
# 561a888 合入 main，这两个常量从此是历史刻度，不再变。
INITIAL_EXCESS = 120_108
INITIAL_OVER_RED = 20


def progress(rows):
    """返回 (已消化比例, 当前超标行数, 当前越红线文件数)。"""
    excess = sum(max(0, row["total"] - TARGET) for row in rows.values())
    over_red = sum(1 for row in rows.values() if row["total"] > RED_LINE)
    done = max(0.0, (INITIAL_EXCESS - excess) / INITIAL_EXCESS)
    return done, excess, over_red


def report(rows):
    totals = [row["total"] for row in rows.values()]
    tests = sum(row["tests"] for row in rows.values())
    print(f"{len(rows)} 个源文件，{sum(totals):,} 行"
          f"（生产 {sum(totals) - tests:,} / 测试 {tests:,}）")
    print(f"  中位数 {statistics.median(totals):.0f}"
          f"   P75 {percentile(totals, 0.75)}"
          f"   P90 {percentile(totals, 0.90)}"
          f"   P95 {percentile(totals, 0.95)}"
          f"   最大 {max(totals)}")
    done, excess, over_red = progress(rows)
    print(f"\n\033[1m拆分进度 {done:.1%}\033[0m"
          f"  超标行 {INITIAL_EXCESS:,} → {excess:,}"
          f"  越红线文件 {INITIAL_OVER_RED} → {over_red}")
    print()
    for label, line in (("目标", TARGET), ("上限", LIMIT), ("红线", RED_LINE)):
        over = [name for name, row in rows.items() if row["total"] > line]
        print(f"  超过{label} {line:>4} 行: {len(over):>3} 个文件"
              f"（{len(over) / len(rows):.0%}）")

    worst = sorted(rows.items(), key=lambda item: -item[1]["total"])
    shown = [item for item in worst if item[1]["total"] > LIMIT]
    if shown:
        print(f"\n{'文件':<48}{'总行':>7}{'测试':>7}{'生产':>7}")
        print("─" * 69)
        for name, row in shown:
            flag = " 🔴" if row["total"] > RED_LINE else ""
            print(f"{name:<48}{row['total']:>7}{row['tests']:>7}"
                  f"{row['total'] - row['tests']:>7}{flag}")


def exists_at_head(name):
    """这个路径在上一个提交里存在吗？"""
    return subprocess.run(
        ["git", "cat-file", "-e", f"HEAD:{name}"],
        capture_output=True, cwd=ROOT,
    ).returncode == 0


def rename_map():
    """{新路径: 原路径}。

    `git mv` 之后新路径不在基线里，直接判成「新增越红线文件」会把搬家当成
    新建——拆分期几乎每一步都在搬文件，这个误报必须消掉。
    """
    mapping = {}
    for args in (
        ["git", "diff", "--cached", "-M", "--name-status"],
        ["git", "diff", "-M", "--name-status", "HEAD"],
    ):
        out = subprocess.run(args, capture_output=True, text=True, cwd=ROOT)
        for line in out.stdout.split("\n"):
            parts = line.split("\t")
            if len(parts) == 3 and parts[0].startswith("R"):
                mapping[parts[2]] = parts[1]
    return mapping


def check(rows, baseline, strict=False):
    problems = []
    base_rows = baseline["files"]
    base_total = sum(row["total"] for row in base_rows.values())
    now_total = sum(row["total"] for row in rows.values())

    renames = rename_map()
    for name, row in rows.items():
        was = base_rows.get(name) or base_rows.get(renames.get(name, ""))
        # 「新文件」的判据是「上一个提交里没有」，不是「基线里没有」。基线是
        # 拆分开始时的快照，文件一路在搬家，早就对不上路径了；而 HEAD 是刚
        # 刚那一步，拿它判才准。
        # `strict`（CI）：干净检出里每个文件都「在上一个提交里」，那条豁免在 CI 上恒成立、
        # 等于没查。CI 改成凡是基线里没有的越红线文件都判红。
        if row["total"] > RED_LINE and was is None and (strict or not exists_at_head(name)):
            problems.append(f"新增越红线文件：{name} {row['total']} 行 > {RED_LINE}")
        elif (
            was
            and was["total"] > LIMIT
            and row["total"] > was["total"] + OVERSIZE_SLACK
        ):
            problems.append(
                f"超标文件变长：{name} {was['total']} → {row['total']} 行"
                f"（容差 {OVERSIZE_SLACK} 行）"
            )

    if now_total > base_total * (1 + GROWTH_TOLERANCE):
        problems.append(
            f"总行数增长过多：{base_total:,} → {now_total:,}"
            f"（+{now_total / base_total - 1:.1%}，容忍 {GROWTH_TOLERANCE:.0%}）"
            "——拆分不该复制代码"
        )

    if problems:
        print("\n门禁未通过：")
        for item in problems:
            print(f"  ✗ {item}")
        return 1
    print(f"\n门禁通过：无新增越线文件，超标文件未变长，"
          f"总行数 {now_total:,}（基线 {base_total:,}）")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-baseline", action="store_true", help="记录当前状态为基线")
    parser.add_argument("--check", action="store_true", help="与基线对比并在恶化时退出非零")
    parser.add_argument("--strict", action="store_true",
                        help="CI 用：基线里没有的越红线文件一律判红，不看上一个提交里有没有")
    args = parser.parse_args()

    rows = collect()
    report(rows)

    if args.write_baseline:
        BASELINE.write_text(
            json.dumps({"files": rows}, ensure_ascii=False, indent=2, sort_keys=True)
            + "\n",
            encoding="utf-8",
        )
        print(f"\n基线已写入 {BASELINE.relative_to(ROOT)}")
        return 0

    if args.check:
        if not BASELINE.exists():
            print(f"\n缺少基线文件 {BASELINE.relative_to(ROOT)}，"
                  "先跑 --write-baseline")
            return 1
        return check(rows, json.loads(BASELINE.read_text(encoding="utf-8")), strict=args.strict)
    return 0


if __name__ == "__main__":
    sys.exit(main())
