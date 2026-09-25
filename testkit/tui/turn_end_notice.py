#!/usr/bin/env python3
"""回合收尾那行 `✻`、收缩行的新说法、后台任务完成那行铃铛点得开（用户 09-26）。

四段，每段一个干净的沙箱：

1. 跑完的一轮：收缩行是「运行了 1 次命令 · 思考了 1 次」（不再有 `Worked for`）；回复之后
   空一行是 `✻ stub-model · <动词> N 秒 · H:MM 完成`；退出重开、切回这条会话，回放里还是
   那一行，动词不变（按轮号固定）。
2. 被打断的一轮：Ctrl+C 之后是 `✻ … 中断`；重开回放也是它，不再另起一行「已中断」。
3. 空闲时后台命令跑完：唤醒轮开头那行铃铛（「命令完成 <id> · 走查后台任务」）点得开，点开是
   「输出结尾：」和命令输出的最后几行；重开回放照样点得开。
4. 回合跑着时后台命令跑完：报告排进这一轮，被吃进去时落成的那行铃铛同样点得开。

    MIYU_BIN=~/.cache/miyu-accept-2026-09-26c/miyu MIYU_HOME=~/.cache/miyu-turn-end/home \\
      MIYU_TUI_PORT=18985 STUB_PORT=18986 MIYU_TUI_RUNTIME=~/.cache/miyu-turn-end/rt \\
      OUT=~/.cache/miyu-turn-end/out python3 testkit/tui/turn_end_notice.py

产物（每段的屏幕文本）在 OUT 下。这些 TUI 走查只能一个一个跑（共用沙箱与端口）。
"""

import json
import os
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import fold_summary as fs  # noqa: E402

REPLY = "好的,收到"
WAKE = re.compile(r"命令完成 \w+ · 走查后台任务")


def now(sink):
    return h.render(bytes(sink))


def save(name, screen):
    (h.OUT / f"turn-end-{name}.txt").write_text("\n".join(screen or []) + "\n", encoding="utf-8")


def end_line(screen):
    return next((line.strip() for line in reversed(screen or []) if fs.is_turn_end(line)), None)


def verb(line):
    """`✻ 模型 · 处理了 3 秒 · 1:53 完成` 里的动词。"""
    part = next((part for part in (line or "").split(" · ") if "秒" in part or re.search(r"\d+s\b", part)), "")
    return part.split(" ")[0] if part else None


def submit(master, sink, text):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")


def reopen(master, sink, tui, name):
    """起名、Ctrl+D 退出，重开 TUI、切回这条会话。返回 (tui, master, sink)。"""
    submit(master, sink, f"/rename {name}")
    h.drain_until(master, sink, "已重命名", 8.0)
    h.settle(master, sink)
    os.write(master, b"\x04")
    h.drain(master, 2.0, sink)
    r.stop(tui)
    tui, master = h.spawn_tui()
    sink = bytearray()
    h.reset_view()
    deadline = time.time() + 20.0
    while time.time() < deadline and not any("Tab" in line for line in now(sink)):
        h.settle(master, sink, quiet=0.2, timeout=1.0)
    submit(master, sink, f"/session {name}")
    return tui, master, sink


def click_line(master, sink, pattern):
    """点屏幕上第一行匹配 `pattern` 的那一行，返回点完之后的屏。"""
    screen = now(sink)
    row = next((i for i, line in enumerate(screen) if pattern.search(line)), None)
    if row is None:
        return None
    h.click(master, sink, 4, row, quiet=0.4, timeout=3.0)
    return now(sink)


def scenario_done(report):
    stub, daemon, tui, master, sink = r.start({
        "STUB_TOOL": "1",
        "STUB_REASONING": "1",
        "STUB_TOOL_COMMAND": "sleep 1; printf 'walk-out\\n'",
        "STUB_CHUNK_SLEEP": "0.02",
    })
    try:
        submit(master, sink, h.PROMPT)
        done = r.wait_screen(master, sink, lambda s: end_line(s) is not None, 60.0)
        save("done", done or r.LAST.get("screen"))
        report["done_end_line_shows_up"] = done is not None
        if done is None:
            return
        line = end_line(done)
        report["_done_end_line"] = line
        report["done_end_line_names_the_model"] = line.startswith("✻ stub-model · ")
        report["done_end_line_says_done"] = line.endswith("完成") and re.search(r"· \d+:\d\d 完成$", line) is not None
        index = max(i for i, row in enumerate(done) if fs.is_turn_end(row))
        reply_at = max((i for i, row in enumerate(done) if REPLY in row), default=None)
        report["done_end_line_follows_the_reply"] = reply_at is not None and reply_at < index
        report["done_blank_line_before_end_line"] = index > 0 and not done[index - 1].strip()
        folds = [row.strip() for row in done if fs.is_fold_summary(row)]
        report["_folds"] = folds
        report["fold_line_uses_new_wording"] = any("运行了 1 次命令" in row for row in folds) and not any(
            "Worked for" in row for row in done
        )
        tui, master, sink = reopen(master, sink, tui, "收尾走查")
        again = r.wait_screen(master, sink, lambda s: end_line(s) is not None and any(REPLY in row for row in s), 30.0)
        save("done-reopened", again or r.LAST.get("screen"))
        replayed = end_line(again)
        report["_replayed_end_line"] = replayed
        report["reopen_keeps_the_end_line"] = replayed is not None and replayed.endswith("完成")
        report["reopen_keeps_the_verb"] = replayed is not None and verb(replayed) == verb(line)
    finally:
        r.stop(tui, daemon, stub)


def scenario_interrupt(report):
    stub, daemon, tui, master, sink = r.start({
        "STUB_TOOL": "1",
        "STUB_TOOL_COMMAND": "sleep 30",
        "STUB_CHUNK_SLEEP": "0.02",
    })
    try:
        submit(master, sink, h.PROMPT)
        running = r.wait_screen(master, sink, lambda s: any("跑个命令" in row or "运行命令" in row for row in s), 30.0)
        report["interrupt_command_running"] = running is not None
        time.sleep(1.0)
        os.write(master, b"\x03")
        cut = r.wait_screen(master, sink, lambda s: (end_line(s) or "").endswith("中断"), 20.0)
        save("interrupt", cut or r.LAST.get("screen"))
        report["interrupt_end_line_says_stopped"] = cut is not None
        report["_interrupt_end_line"] = end_line(cut)
        if cut is None:
            return
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        tui, master, sink = reopen(master, sink, tui, "中断走查")
        again = r.wait_screen(master, sink, lambda s: end_line(s) is not None, 30.0)
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        again = now(sink)
        save("interrupt-reopened", again)
        replayed = end_line(again)
        report["_interrupt_replayed"] = replayed
        report["interrupt_reopen_end_line_says_stopped"] = (replayed or "").endswith("中断")
        report["interrupt_reopen_has_no_separate_notice"] = not any("已中断" in row and "✻" not in row for row in again)
    finally:
        r.stop(tui, daemon, stub)


def scenario_idle_notice(report):
    stub, daemon, tui, master, sink = r.start({
        "STUB_BACKGROUND": "1",
        "STUB_BACKGROUND_COMMAND": "printf 'BGTAIL-1\\nBGTAIL-2\\n'; sleep 2",
        "STUB_CHUNK_SLEEP": "0.02",
    })
    try:
        submit(master, sink, h.PROMPT)
        woke = r.wait_screen(master, sink, lambda s: any(WAKE.search(row) for row in s), 40.0)
        h.settle(master, sink, quiet=1.0, timeout=10.0)
        save("idle-notice", now(sink))
        report["idle_notice_shows_up"] = woke is not None
        if woke is None:
            return
        opened = click_line(master, sink, WAKE)
        save("idle-notice-open", opened)
        report["idle_notice_expands_to_the_output_tail"] = bool(opened) and any(
            "BGTAIL-2" in row for row in opened
        ) and any("输出结尾" in row for row in opened)
        tui, master, sink = reopen(master, sink, tui, "铃铛走查")
        r.wait_screen(master, sink, lambda s: any(WAKE.search(row) for row in s), 30.0)
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        opened = click_line(master, sink, WAKE)
        save("idle-notice-reopened-open", opened)
        report["idle_notice_expands_after_reopen"] = bool(opened) and any("BGTAIL-2" in row for row in opened)
    finally:
        r.stop(tui, daemon, stub)


def scenario_queued_notice(report):
    stub, daemon, tui, master, sink = r.start({
        "STUB_TOOL": "1",
        "STUB_TOOL_ROUNDS": "2",
        "STUB_TOOL_COMMAND": "sleep 6",
        "STUB_BACKGROUND_COMMAND": "printf 'QTAIL-1\\nQTAIL-2\\n'; sleep 1",
        "STUB_CHUNK_SLEEP": "0.02",
    })
    try:
        submit(master, sink, "STUB_BG 开工")
        consumed = r.wait_screen(master, sink, lambda s: any(WAKE.search(row) and "┃" not in row for row in s), 60.0)
        report["queued_notice_shows_up"] = consumed is not None
        r.wait_screen(master, sink, lambda s: end_line(s) is not None, 40.0)
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        save("queued-notice", now(sink))
        if consumed is None:
            return
        opened = click_line(master, sink, WAKE)
        save("queued-notice-open", opened)
        report["queued_notice_expands_to_the_output_tail"] = bool(opened) and any(
            "QTAIL-2" in row for row in opened
        )
    finally:
        r.stop(tui, daemon, stub)


def main():
    report = {}
    only = os.environ.get("ONLY")
    for name, scenario in (
        ("done", scenario_done),
        ("interrupt", scenario_interrupt),
        ("idle", scenario_idle_notice),
        ("queued", scenario_queued_notice),
    ):
        if only and name not in only.split(","):
            continue
        try:
            scenario(report)
        except Exception as error:  # 一段挂了照样跑下一段，报告里看得见是哪段。
            report[f"{name}_crashed"] = False
            report[f"_{name}_error"] = repr(error)
    return report


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    checks = {k: v for k, v in report.items() if not k.startswith("_") and v is not None}
    passed = sum(1 for v in checks.values() if v)
    print(f"{passed}/{len(checks)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if checks and passed == len(checks) else 1)
