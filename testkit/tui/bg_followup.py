#!/usr/bin/env python3
"""后台任务在**回合还在跑的时候**完成：它的报告会打乱前台渲染吗。

用户 09-21 报：跑着一个长回合，后台命令跑完了，屏幕上冒出
`[后台任务完成] 命令完成 7f70b8 · …` 一条粉色用户气泡，而且从那以后正文就乱了
——好几行「思考中 · N 词元」叠在一起（正常只该有正在进行的那一行），以及
一大片空洞（正文和转轮行之间隔着几十行空白）。

daemon 那边这是设计内的：会话上有活跃轮时，后台完成的报告不另起唤醒轮，而是
当 follow-up 排进正在跑的那一轮（`web/actor/job_wake.rs`）。所以要复现就得让
后台命令**在主线回合跑着的时候**完成。

    cargo build
    python3 testkit/tui/bg_followup.py

产物在 ~/.cache/miyu-tui-smoke/bgfollow-*.txt。这些 TUI 走查只能一个一个跑
（共用 MIYU_HOME 与桩模型端口）。
"""

import os
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 「思考中 · 50 词元 · 2.3s」那一行。正常只有 live 区那一行长这样。
THINKING = re.compile(r"思考中\s*·")
# 后台报告那一行，不管它被画成什么样子。
WAKE = re.compile(r"(后台任务完成|命令完成|子代理完成)")


def body_rows(screen):
    """正文区：砍掉屏幕底部的输入框与状态行。

    正文短的时候，正文和输入框之间本来就是一片空地——把它算进来就永远判不出
    「时间线中间破了个洞」（用户图 6 的形状：正文在上、转轮行贴着输入框，中间
    空二十来行）。"""
    rows = list(screen)
    # 后台任务的状态条挂在 footer **下面**，从底部往上砍会被它挡住。先按
    # footer（带 `tok/s` 的那行）把下半截整个切掉。
    for index in range(len(rows) - 1, -1, -1):
        if "tok/s" in rows[index] or rows[index].lstrip().startswith("┃"):
            rows = rows[:index]
            break
    while rows and (not rows[-1].strip() or rows[-1].lstrip().startswith("┃")):
        rows.pop()
    return rows


def longest_blank_run(screen):
    """正文**内部**最长的一段空白。屏幕下方那片空地不算——正文短的时候本来
    就是空的，算进来判不出「正文中间破了个洞」。"""
    rows = list(screen)
    while rows and not rows[-1].strip():
        rows.pop()
    best = current = 0
    for line in rows:
        if line.strip():
            current = 0
            continue
        current += 1
        best = max(best, current)
    return best


BRAILLE = set("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")


def turn_is_running(screen):
    """回合还在跑：屏幕上有转轮那一行。回合收尾之后下半屏本来就是空的，
    那种空白不算空洞。"""
    return any(line.lstrip()[:1] in BRAILLE for line in screen if line.strip())


def save(name, screen):
    (h.OUT / f"bgfollow-{name}.txt").write_text(
        "\n".join(screen) + "\n", encoding="utf-8"
    )


def sample(master, sink, seconds, shots):
    """盯着屏幕采样，只留回合还在跑的那些屏（判据按最坏的一屏算）。"""
    deadline = time.time() + seconds
    while time.time() < deadline:
        r.wait_screen(master, sink, lambda _s: False, 0.3)
        screen = r.LAST["screen"]
        if screen and turn_is_running(screen):
            shots.append(screen)
    return shots


def main():
    report = {}
    # daemon 默认只记 error，那条「follow-up 走了哪条路」是 info。
    h.ENV["MIYU_LOG"] = "info"
    # 后台命令 6 秒跑完；主线要跑得更久（8 轮 × 2 秒），它完成时回合还在跑。
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        # 思考正文用**英文长行**：用户那次的思考是英文，一行折出来正好贴着
        # 终端右缘，宽度算差一列就软折行——而软折行会让转轮走「擦了重画」
        # 那条路，图 4 里那几行叠着的「思考中」怀疑就出在这儿。
        # **不带空格**的长串：折行只能硬切，切出来的每一行都正好是可用宽度的
        # 满宽。满宽的行会让终端停在「待换行」状态，那正是怀疑出错位的地方。
        "STUB_REASONING_TEXT": ("abcdefghij" * 40),
        "STUB_TOOL": "1",
        "STUB_TOOL_ROUNDS": "4",
        "STUB_TOOL_COMMAND": "sleep 5",
        "STUB_BACKGROUND_COMMAND": "sleep 3; echo bg-done",
        "STUB_CHUNK_SLEEP": "0.05",
    })
    shots = []
    try:
        message = "STUB_BG 开工"
        os.write(master, message.encode())
        h.drain_until(master, sink, message, 5.0)
        os.write(master, b"\r")
        # 后台任务派出去了（状态条上有它）。
        screen = r.wait_screen(
            master, sink,
            lambda s: any("走查后台任务" in line for line in s),
            30.0,
        )
        report["bg01_background_job_started"] = screen is not None
        save("started", screen or r.LAST["screen"] or [])
        # 后台命令 6 秒后完成 → 报告作为 follow-up 插进正在跑的这一轮。
        screen = r.wait_screen(
            master, sink,
            lambda s: any(WAKE.search(line) for line in s),
            40.0,
        )
        report["bg02_wake_report_shows_up"] = screen is not None
        save("wake", screen or r.LAST["screen"] or [])
        wake_line = ""
        if screen:
            wake_line = next(line for line in screen if WAKE.search(line))
            (h.OUT / "bgfollow-wake-line.txt").write_text(
                wake_line + "\n", encoding="utf-8"
            )

        # 报告插进来之后再看 20 秒：正文该继续正常流。
        sample(master, sink, 20.0, shots)
        shots = [s for s in shots if s]
        worst_thinking = 0
        worst_blank = 0
        worst_shot = None
        for shot in shots:
            body = body_rows(shot)
            worst_thinking = max(
                worst_thinking, sum(1 for line in body if THINKING.search(line))
            )
            blank = longest_blank_run(body)
            if blank > worst_blank:
                worst_blank = blank
                worst_shot = shot
        if worst_shot:
            save("worst-shot", worst_shot)
        report["bg03_no_stacked_thinking_rows"] = worst_thinking <= 1
        report["bg04_no_blank_hole_in_body"] = worst_blank <= 5
        (h.OUT / "bgfollow-worst.txt").write_text(
            f"thinking_rows={worst_thinking} blank_run={worst_blank}\n",
            encoding="utf-8",
        )
        # 消费之后再看它长什么样：排着队的时候是气泡（和用户自己排队一个样，
        # 那是对的），被这一轮吃进去之后该变成时间线上的一条通知——没有用户
        # 气泡那条竖条，也不把内部抬头 `[后台任务完成]` 原样端到脸上。
        final = r.wait_screen(master, sink, lambda _s: False, 3.0) or r.LAST["screen"] or []
        save("final", final)
        consumed = [line for line in final if WAKE.search(line)]
        report["bg06_report_is_not_a_user_bubble"] = bool(consumed) and all(
            "┃" not in line and "[后台任务完成]" not in line for line in consumed
        )
        # 版式（用户 09-21 定）：这一轮先收成 `Worked for …`，通知行在**它下面**，
        # 再空一行才接着说。
        notice_at = next(
            (i for i, line in enumerate(final) if WAKE.search(line)), None
        )
        worked_at = next(
            (
                i
                for i, line in enumerate(final)
                if h.is_fold_summary(line) and (notice_at is None or i < notice_at)
            ),
            None,
        )
        report["bg07_notice_sits_under_the_summary"] = (
            notice_at is not None and worked_at is not None and worked_at < notice_at
        )
        report["bg08_blank_line_after_the_notice"] = (
            notice_at is not None
            and notice_at + 1 < len(final)
            and not final[notice_at + 1].strip()
        )
        if shots:
            save("last", shots[-1])
        # 整屏擦（`ESC[2J`）在回合流着的时候每多一次就是看得见的一闪。主走查
        # item13 的阈值是 12，这里只盯这一段、给出确切的数，好定位是哪一处。
        clears = bytes(sink).count(b"\x1b[2J")
        (h.OUT / "bgfollow-clears.txt").write_text(
            f"full_clears={clears}\n", encoding="utf-8"
        )
        report["bg09_few_full_screen_clears"] = clears <= 12
    finally:
        r.stop(tui, daemon, stub)

    # daemon 的详细日志在沙箱家目录里（启动输出那份只有 WebUI 地址）。
    logs = sorted((h.HOME / "cache" / "logs").glob("miyu.*.log"))
    log = "".join(
        path.read_text(encoding="utf-8", errors="replace") for path in logs
    )
    report["bg05_daemon_took_the_followup_path"] = (
        "job wake joining the session's active run" in log
    )

    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
