#!/usr/bin/env python3
"""大厅里按 Tab 换车道只换显示，真用会话时才换（用户 09-23）真机走查。

- 按 Tab：一帧换到开发模式，库里不多一条会话；上下文直接是开发车道开一条新会话时
  的数（启动时后台事先问好的，用户 09-24），不是「—」；再按一下回到普通模式，上下文
  还是原来那个数。
- 按 Tab 换到开发、再敲 `/normal`：回普通车道，途中不白开开发会话。
- 按 Tab 换到开发、发第一句：这一句落在开发模式的会话里；回车那一帧直接就是正文
  布局，中间不再画一帧空大厅（用户 09-23「回车提交会闪一下」）。
- 新开一条开发会话，按 Tab 换到普通、再按 Shift+Tab 开只读：只读开在普通车道的会话上；
  会话这时才真建出来，footer 上的数不跳（事先算的和建出来报的是同一套）。
- 开发车道同样对一遍（另起一个干净进程，免得用上前面记下的数）：Tab 过去看到的是
  daemon 事先算的，Shift+Tab 真建出开发会话后报的数要和它一样。

    cargo build
    python3 testkit/tui/lobby_lane.py
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
import latency_probe as lp  # noqa: E402

import pyte  # noqa: E402

STUB = {"STUB_CHUNK_SLEEP": "0.01"}
DEV_ON = lp.DEV_ON
NORMAL_ON = lp.NORMAL_ON
# 窗口是猜的时候分母前带 `~`（`10.7k/~168k`）。
CONTEXT = re.compile(r"(—|[\d.]+k?)/~?[\d.]+[kM]")


def footer_context(screen):
    """footer 上的上下文读数：「—」或者「10.7k」这样的数。"""
    for line in reversed(screen):
        if "stub-model" in line:
            match = CONTEXT.search(line)
            return match.group(1) if match else None
    return None


def sessions():
    """库里两条车道的会话（`all`；不带只列当前人格的）。`mode` 不报就是普通，
    回合数的键是 `turn_count`。"""
    _, frame = lp.ipc({"command": "list_sessions", "mode": "all"})
    rows = frame.get("data", {}).get("sessions", [])
    for row in rows:
        row.setdefault("mode", "normal")
    return rows


def session_state(session_id):
    _, frame = lp.ipc({"command": "get_session_state",
                       "target": {"kind": "id", "id": session_id}, "cwd": None})
    return frame.get("state", {}) if isinstance(frame, dict) else {}


def lobby_shown(screen):
    return any("A G E N T" in line for line in screen)


def key(master, sink, data, predicate, timeout=5.0):
    """按键，等画面满足 predicate 且 footer 那行读得出数；返回 (毫秒, 满足时那一屏)，
    超时毫秒为 None。

    返回的是满足条件的那一屏，不是事后再截的：大厅动画一直在出帧，`settle` 等不到
    安静只会超时，那时截到的可能是半帧（footer 只画到一半，数读成空）。
    """
    started = time.time()
    os.write(master, data)
    screen = r.wait_screen(
        master, sink, lambda s: predicate(s) and footer_context(s) is not None, timeout
    )
    elapsed = round((time.time() - started) * 1000) if screen is not None else None
    h.settle(master, sink, quiet=0.4, timeout=5)
    return elapsed, screen if screen is not None else h.render(bytes(sink))


def command(master, sink, text, quiet=0.8):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=20)
    return h.render(bytes(sink))


def frames_since(raw, start):
    """从 `start` 字节起，每个同步块收尾（或块外显示光标）时屏上是什么样。"""
    screen = pyte.Screen(h.COLS, h.ROWS)
    stream = pyte.ByteStream(screen)
    stream.feed(raw[:start])
    token = re.compile(rb"\x1b\[\?2026h|\x1b\[\?2026l|\x1b\[\?25h")
    depth, pos, frames = 0, start, []
    for match in token.finditer(raw, start):
        seq = match.group(0)
        boundary = False
        if seq == b"\x1b[?2026h":
            depth += 1
        elif seq == b"\x1b[?2026l":
            depth = max(0, depth - 1)
            boundary = depth == 0
        elif depth == 0:
            boundary = True
        if boundary:
            stream.feed(raw[pos:match.end()])
            pos = match.end()
            try:
                frames.append(list(screen.display))
            except IndexError:
                pass
    return frames


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        screen = r.wait_screen(master, sink, lambda s: lobby_shown(s) and footer_context(s), 20)
        r.save("lobby-lane-start", screen or r.LAST["screen"] or [])
        start_context = footer_context(screen or [])
        before = sessions()
        report["starts_in_normal_lobby"] = bool(screen) and any(NORMAL_ON.search(l) for l in screen)

        # 1. Tab：只换显示
        took, screen = key(master, sink, b"\t", lambda s: any(DEV_ON.search(l) for l in s))
        r.save("lobby-lane-tab-dev", screen)
        report["tab_switch_ms"] = took
        report["tab_shows_dev"] = took is not None
        report["tab_dev_context"] = footer_context(screen)
        report["tab_dev_context_is_known"] = footer_context(screen) not in (None, "—")
        report["tab_creates_no_session"] = [s["session_id"] for s in sessions()] == [
            s["session_id"] for s in before
        ]
        took, screen = key(master, sink, b"\t", lambda s: any(NORMAL_ON.search(l) for l in s))
        report["tab_back_shows_normal"] = took is not None
        report["tab_back_keeps_the_number"] = footer_context(screen) == start_context

        # 2. Tab 到开发，敲 /normal：回普通车道，不白开开发会话
        key(master, sink, b"\t", lambda s: any(DEV_ON.search(l) for l in s))
        screen = command(master, sink, "/normal")
        r.save("lobby-lane-normal-cmd", screen)
        report["normal_cmd_returns_to_normal"] = any(NORMAL_ON.search(l) for l in screen)
        report["normal_cmd_creates_no_dev_session"] = not any(
            s.get("mode") == "dev" for s in sessions()
        )

        # 3. Tab 到开发，发第一句
        key(master, sink, b"\t", lambda s: any(DEV_ON.search(l) for l in s))
        text = "开发车道第一句"
        os.write(master, text.encode())
        h.drain_until(master, sink, text, 3.0)
        mark = len(sink)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.2, timeout=40)
        screen = h.render(bytes(sink))
        r.save("lobby-lane-first-message", screen)
        # `mark` 之后头几帧可能还是回车被读到之前的大厅动画，不算。旧版闪的那一帧是
        # 「大厅还在、输入框里的字已经没了」：提交回显了，大厅要等开回合回来才撤。
        frames = frames_since(bytes(sink), mark)
        in_lobby = [any("A G E N T" in line for line in frame) for frame in frames]
        left = next((i for i, lobby in enumerate(in_lobby) if not lobby), None)
        report["enter_frames"] = len(frames)
        report["enter_leaves_lobby"] = left is not None
        report["enter_no_empty_lobby_frame"] = not any(
            lobby and not any(text in line for line in frame)
            for lobby, frame in zip(in_lobby, frames)
        )
        report["enter_lobby_never_comes_back"] = left is not None and not any(in_lobby[left:])
        dev_rows = [s for s in sessions() if s.get("mode") == "dev"]
        report["first_message_lands_in_dev"] = any(s.get("turn_count", 0) >= 1 for s in dev_rows)
        report["footer_dev_has_a_number"] = footer_context(screen) not in (None, "—")

        # 4. 新开一条开发会话，Tab 到普通，Shift+Tab 开只读：开在普通车道的会话上
        screen = command(master, sink, "/new")
        report["new_dev_lobby"] = lobby_shown(screen) and any(DEV_ON.search(l) for l in screen)
        _, screen = key(master, sink, b"\t", lambda s: any(NORMAL_ON.search(l) for l in s))
        before_materialize = footer_context(screen)
        report["tab_normal_context_is_known"] = before_materialize not in (None, "—")
        took, screen = key(master, sink, b"\x1b[Z",
                           lambda s: any("只读" in l and "stub-model" in l for l in s), 8.0)
        report["materialize_keeps_the_number"] = footer_context(screen) == before_materialize
        report["_context_before_after"] = [before_materialize, footer_context(screen)]
        r.save("lobby-lane-readonly", screen)
        report["shift_tab_turns_read_only_on"] = took is not None
        report["read_only_stays_normal"] = any(NORMAL_ON.search(l) for l in screen)
        normal_rows = [s for s in sessions() if s.get("mode") == "normal"]
        report["read_only_is_on_a_normal_session"] = any(
            session_state(s["session_id"]).get("sandbox_readonly") for s in normal_rows
        )
        report["read_only_not_on_dev_sessions"] = not any(
            session_state(s["session_id"]).get("sandbox_readonly")
            for s in sessions() if s.get("mode") == "dev"
        )
        return report
    finally:
        r.stop(tui, daemon, stub)


def dev_materialize(report):
    """开发车道：Tab 过去那一刻的数（事先算的）和真建出开发会话之后的数一致。

    要一个干净的进程：Tab 回来过的车道会记下自己屏上的数，再切过去用的是记下的那个，
    量不到事先算的。
    """
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        r.wait_screen(master, sink, lambda s: lobby_shown(s) and footer_context(s), 20)
        _, screen = key(master, sink, b"\t", lambda s: any(DEV_ON.search(l) for l in s))
        shown = footer_context(screen)
        took, screen = key(master, sink, b"\x1b[Z",
                           lambda s: any("只读" in l and "stub-model" in l for l in s), 8.0)
        r.save("lobby-lane-dev-materialize", screen)
        report["dev_materialized"] = took is not None and any(
            s.get("mode") == "dev" for s in sessions()
        )
        report["dev_tab_number_matches_materialized"] = (
            shown not in (None, "—") and footer_context(screen) == shown
        )
        report["_dev_context_tab_materialized"] = [shown, footer_context(screen)]
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    dev_materialize(report)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
