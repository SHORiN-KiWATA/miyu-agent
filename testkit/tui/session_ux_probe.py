#!/usr/bin/env python3
"""09-25 用户实测切子会话的几处问题：复现在前，修完当回归用例。

第 1 轮桩回一段长回复把主会话上下文撑大；换一份桩配置跑第 2 轮：派两个子代理（两条都
sleep 90）。09-26 起子代理只在后台跑，主线派完这一轮就收。然后：
1. 进其中一个子会话：量耗时；任务条上看不看得到兄弟（另一条）；
2. 子会话里 `/session`：光标停在哪一行、有没有「*」当前标记；
3. 子会话里 `/compact` 一次：提示是什么；
4. `/back`：量耗时；回来当场就是主会话、任务条上还列着在跑的子代理；回来前后 footer 上下文数；
5. （回合跑着时 `/compact` 像插话一样排队，这条原来靠主线挂着等子代理，只后台之后主线不挂着了，
   由 `midturn_commands` 的「一b」接着测。）
6. 主会话里 `/session` 按 Ctrl+D：名下还有子代理在跑的当前会话第一下只出提醒；第二下连同子代理树
   删干净，面板留着（落到兜底会话上）。

    MIYU_BIN=... MIYU_HOME=~/.cache/miyu-session-ux/home MIYU_TUI_PORT=18961 STUB_PORT=18962 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-session-ux/rt OUT=~/.cache/miyu-session-ux/out \\
      python3 testkit/tui/session_ux_probe.py
"""

import json
import os
import re
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402
import subagent_visit as sv  # noqa: E402

BIG = "这一段是用来把上下文撑大的回复正文。" * 1500
ENV_A = {
    "STUB_REPLY": BIG,
    "STUB_CHUNK_CHARS": "4000",
    "STUB_CHUNK_SLEEP": "0.001",
    "STUB_USAGE_BY_SIZE": "1",
}
# 桩的阶段表按「请求里已有几条工具结果」取：后台子代理那条结果占了第 0 格，
# STUB_ASK 垫在第 0 格，前台子代理才落在第 1 格。
ENV_B = {
    "STUB_ASK": "1",
    "STUB_SUBAGENT": "1",
    "STUB_SUBAGENT_COMMAND": "sleep 90; printf 'SUBOUT\\n'",
    "STUB_SUBAGENT_BG_COMMAND": "sleep 90; printf 'BGOUT\\n'",
    "STUB_SUBAGENT_BG_ROUNDS": "1",
    "STUB_CHUNK_SLEEP": "0.02",
    "STUB_USAGE_BY_SIZE": "1",
}
BG_ROW = "走查后台子代理"
CONTEXT = re.compile(r"(\d+(?:\.\d+)?)([kKM]?)/~?(\d+(?:\.\d+)?[kKM]?)")


def footer_context(screen):
    """footer 上「12.3k/128k(9.6%)」那一段，换成词元数。"""
    for line in reversed(screen):
        match = CONTEXT.search(line)
        if match:
            scale = {"k": 1e3, "K": 1e3, "M": 1e6}.get(match.group(2), 1)
            return round(float(match.group(1)) * scale)
    return None


def swap_stub(stub, env):
    r.stop(stub)
    fresh = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), **env),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
        raise RuntimeError("桩模型没起来")
    return fresh


def say(master, sink, text):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")


def timed(master, sink, keys, predicate, timeout=15.0):
    """按下 `keys` 到屏幕满足 `predicate` 用了多久（秒），外加那一屏。"""
    start = time.monotonic()
    os.write(master, keys)
    screen = r.wait_screen(master, sink, predicate, timeout)
    return (round(time.monotonic() - start, 3) if screen else None), screen


def now(sink):
    """此刻的整屏（现渲染）。`r.LAST` 只在 `wait_screen` 里更新，`settle` 之后读它是旧屏。"""
    return h.render(bytes(sink))


def cursor_row(screen):
    """面板里光标那一行（面板行都带 `┃ ` 前缀）。"""
    for line in screen:
        body = line.strip().removeprefix("┃").strip()
        if body.startswith("›"):
            return body
    return None


NOTE_WORDS = ("busy", "忙", "压缩", "compact", "说完", "Compact")


def clear_input(master, sink):
    os.write(master, b"\x7f" * 40)
    h.settle(master, sink, quiet=0.4, timeout=2.0)


def notes_about(screen, words):
    return [line.strip() for line in screen if any(word in line for word in words)]


def sessions_in_db():
    found = sorted(h.HOME.glob("**/conversation.db"))
    if not found:
        return None
    con = sqlite3.connect(f"file:{found[0]}?mode=ro", uri=True)
    try:
        return [
            {"kind": kind, "child": bool(parent), "turns": turns}
            for kind, parent, turns in con.execute(
                "SELECT kind, parent_session_id, (SELECT count(*) FROM turns t WHERE t.session_id = s.session_id)"
                " FROM sessions s ORDER BY created_at"
            )
        ]
    finally:
        con.close()


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(ENV_A)
    try:
        say(master, sink, h.PROMPT + " 一")
        r.wait_screen(master, sink, lambda s: "撑大" in "\n".join(s), 30.0)
        h.settle(master, sink, quiet=1.5, timeout=20.0)
        report["_context_after_turn1"] = footer_context(now(sink))

        stub = swap_stub(stub, ENV_B)
        say(master, sink, h.PROMPT + " STUB_SUBBG")
        screen = r.wait_screen(
            master, sink,
            # 09-26 起子代理行首是空心 `○`，命令才是转轮。
            lambda s: sv.strip_row(s, sv.ROW) is not None
            and s[sv.strip_row(s, sv.ROW)].lstrip()[:1] == "○",
            40.0,
        )
        report["child_running"] = screen is not None
        if screen is None:
            r.save("ux-no-child", now(sink))
            return report
        h.settle(master, sink, quiet=1.0, timeout=6.0)
        before = now(sink)
        r.save("ux-before", before)
        report["_context_before_visit"] = footer_context(before)
        report["_strip_rows_in_parent"] = [line.strip() for line in before[-8:] if line.strip()]

        # 1. 进子会话（↓ 停在任务条那一行，回车）
        os.write(master, b"\x1b[B")
        h.settle(master, sink, quiet=0.3, timeout=1.5)
        seconds, screen = timed(master, sink, b"\r", sv.inside_child)
        report["_enter_child_seconds"] = seconds
        r.save("ux-in-child", screen or now(sink))
        report["entered_child"] = screen is not None
        if screen is None:
            return report
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        in_child = now(sink)
        report["_context_in_child"] = footer_context(in_child)
        report["sibling_listed_in_child"] = any(BG_ROW in line for line in in_child[-10:])
        report["_strip_rows_in_child"] = [line.strip() for line in in_child[-8:] if line.strip()]

        # 2. 子会话里 `/session`
        sv.command(master, sink, "/session", timeout=3.0)
        panel = r.wait_screen(master, sink, lambda s: any("选择会话" in line for line in s), 8.0)
        r.save("ux-session-from-child", panel or now(sink))
        if panel is not None:
            rows = [line.strip() for line in panel if line.strip().startswith(("›", "*", "  "))]
            report["_picker_cursor_from_child"] = cursor_row(panel)
            report["picker_marks_current_from_child"] = any(
                line.strip().removeprefix("┃").strip().lstrip("› ").startswith("*") for line in panel
            )
            report["_picker_rows_from_child"] = rows[:6]
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.5, timeout=3.0)

        # 3. 子会话里 `/compact`：回合中被拦的命令原样留在输入框里，量完清掉。
        sv.command(master, sink, "/compact", quiet=1.0, timeout=6.0)
        report["_compact_in_child"] = notes_about(now(sink), NOTE_WORDS)[-3:]
        clear_input(master, sink)

        # 4. `/back`
        os.write(master, b"/back")
        h.drain_until(master, sink, "/back", 3.0)
        seconds, screen = timed(master, sink, b"\r", sv.back_in_parent)
        report["_back_seconds"] = seconds
        # 回来那一刻就是主会话，任务条上还列着在跑的那个子代理。
        report["back_shows_parent_and_running_child"] = (
            screen is not None and sv.strip_row(screen, sv.ROW) is not None
        )
        r.save("ux-back-immediate", screen or [])
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        after = now(sink)
        r.save("ux-back", after)
        report["_context_after_back"] = footer_context(after)
        before_ctx, after_ctx = report["_context_before_visit"], report["_context_after_back"]
        report["footer_context_survives_visit"] = (
            before_ctx is not None and after_ctx is not None and after_ctx >= before_ctx * 0.9
        )

        # 5. 主会话里 `/session` → Ctrl+D：第一下只出提醒
        report["_db_before_delete"] = sessions_in_db()
        sv.command(master, sink, "/session", timeout=3.0)
        panel = r.wait_screen(master, sink, lambda s: any("选择会话" in line for line in s), 8.0)
        r.save("ux-session-from-parent", panel or now(sink))
        report["_picker_cursor_from_parent"] = cursor_row(panel or [])
        os.write(master, b"\x04")
        armed = r.wait_screen(master, sink, lambda s: any("再按 Ctrl+D" in line for line in s), 5.0)
        r.save("ux-after-first-ctrl-d", armed or now(sink))
        report["first_ctrl_d_only_warns"] = armed is not None
        report["running_session_survives_one_ctrl_d"] = sessions_in_db() == report["_db_before_delete"]
        # 第二下：删干净，面板留着
        os.write(master, b"\x04")
        gone = r.wait_screen(
            master, sink,
            lambda s: any("选择会话" in line for line in s) and not any("撑大" in line and "›" in line for line in s),
            15.0,
        )
        h.settle(master, sink, quiet=1.0, timeout=6.0)
        r.save("ux-after-second-ctrl-d", now(sink))
        after = sessions_in_db() or []
        report["_db_after_delete"] = after
        report["second_ctrl_d_deletes_the_whole_tree"] = not any(
            row["turns"] >= 2 or row["kind"] == "subagent" for row in after
        )
        report["panel_stays_open_after_deleting_the_current_session"] = gone is not None and any(
            "选择会话" in line for line in now(sink)
        )
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.5, timeout=3.0)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    checks = {key: ok for key, ok in report.items() if not key.startswith("_")}
    passed = sum(1 for ok in checks.values() if ok)
    print(f"{passed}/{len(checks)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if checks and passed == len(checks) else 1)
