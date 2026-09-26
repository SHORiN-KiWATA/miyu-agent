#!/usr/bin/env python3
"""回合跑着的时候执行斜杠命令（用户 09-20：「应该区分能执行和不能执行的命令」）。

原来回合中**所有**命令一律静默吞掉，连 `/new` 都做不到，屏幕上还一点反应都没
有。现在按 `slash_commands::during_turn` 分三档：

- `Inline`：`/goal` 就地执行，跟随一点都不断（它完全不往屏幕上写）；
- `Detach`：要占屏的先把这一轮**分离到后台**，执行完再按事件号挂回来接着看 ——
  已经看过的那半截不会再来一遍；
- `Blocked`：做不了的**说一句为什么**，输入原样留在框里，这一轮说完再回车（`/undo`）；
- `Queue`：像插话一样排进这一轮（09-25，`/compact`）：输入框上方挂一行「排队中」，回合走到
  下一个检查点（这里是说完那一刻）压完就撤。

    cargo build
    python3 testkit/tui/midturn_commands.py
"""

import json
import os
import sqlite3
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 回合要够长，才来得及在它说话的**中途**敲命令。
#
# 第一版只重复 10 遍（约 5 秒），被拦的那条命令回车落在回合**结束之后**，
# 于是它真的执行了，走查以为「Blocked 没生效」——判据自己没造出现场。
LONG = "这一段是为了把回合拖长，好让人来得及在中途敲命令。" * 200
STUB = {
    "STUB_CHUNK_SLEEP": "0.05",
    "STUB_CHUNK_CHARS": "6",
    "STUB_REPLY": LONG,
}


def wait_for(master, sink, predicate, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        h.drain(master, 0.2, sink)
        screen = h.render(bytes(sink))
        if predicate(screen):
            return screen
        time.sleep(0.05)
    return h.render(bytes(sink))


def start_turn(master, sink, text):
    """发一句，等它**开始**流（不是等它说完）。"""
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    return wait_for(
        master,
        sink,
        lambda screen: any("这一段是为了把回合拖长" in line for line in screen),
        30.0,
    )


def turn_running(master, sink):
    """**屏幕还在不在变** —— 这是「回合还在跑」最直接的证据。

    走过两条弯路：看屏幕上有没有那句标志话（跑完也还在，永远为真，等于没
    判）；查库里 status='running'（只读连主库看不见 WAL 里的活数据，永远为
    假）。还在流就一定还在画，直接比两帧。
    """
    before = "\n".join(h.render(bytes(sink)))
    h.drain(master, 0.7, sink)
    return "\n".join(h.render(bytes(sink))) != before


def type_command(master, sink, text):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=0.8, timeout=25)
    return h.render(bytes(sink))


def lane_sessions(persona="default"):
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return []
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        return connection.execute(
            "SELECT session_id FROM sessions WHERE persona = ? AND kind = 'user'"
            "   AND session_id != 'default'",
            (persona,),
        ).fetchall()
    finally:
        connection.close()


def turn_finished(session_id=None):
    """库里有没有跑完的轮 —— 判「分离之后它在 daemon 里跑完了」。"""
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return 0
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        if session_id:
            return connection.execute(
                "SELECT COUNT(*) FROM turns WHERE session_id = ? AND status = 'completed'",
                (session_id,),
            ).fetchone()[0]
        return connection.execute(
            "SELECT COUNT(*) FROM turns WHERE status = 'completed'"
        ).fetchone()[0]
    finally:
        connection.close()


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        # ── 一、Blocked：说一句为什么，输入留在框里 ──
        start_turn(master, sink, "第一句")
        report["回合真的在流"] = turn_running(master, sink)
        # 先把字打进去，**确认这一刻还在流**再按回车——不然按下去的时候回合
        # 已经结束，测的就不是「回合中」了（第一版就是这么假报红的）。
        os.write(master, b"/undo")
        h.drain_until(master, sink, "/undo", 5.0)
        report["按回车那一刻回合还在跑"] = turn_running(master, sink)
        os.write(master, b"\r")
        # **不要 settle**：settle 会一直等到流安静，那就等成了「回合结束之后」，
        # 现场就没了。固定等一小会儿，趁回合还在跑的时候看屏。
        h.drain(master, 1.5, sink)
        screen = h.render(bytes(sink))
        r.save("midturn-blocked", screen)
        report["被挡下来时回合确实还在跑"] = turn_running(master, sink)
        report["做不了的会说一句为什么"] = any(
            "等这一轮说完" in line or "wait for it to finish" in line for line in screen
        )
        report["被挡下来输入还留在框里"] = any("/undo" in line for line in screen)
        # 把它退掉，免得影响后面。
        os.write(master, b"\x7f" * 10)
        h.drain(master, 0.8, sink)

        # ── 一b、Queue：/compact 像插话一样排进这一轮 ──
        os.write(master, b"/compact")
        h.drain_until(master, sink, "/compact", 5.0)
        report["敲 /compact 那一刻回合还在跑"] = turn_running(master, sink)
        os.write(master, b"\r")
        h.drain(master, 1.5, sink)
        screen = h.render(bytes(sink))
        r.save("midturn-queued-compact", screen)
        report["压缩排进这一轮"] = any("排队中" in line or "Queued" in line for line in screen)
        report["排队之后输入框空了"] = not any(
            line.strip().startswith("┃ /compact") and "排队中" not in line for line in screen[-6:]
        )
        # 让这一轮说完：压缩在说完那一刻做掉，排队那一行撤掉。
        h.settle(master, sink, quiet=2.0, timeout=180)
        screen = h.render(bytes(sink))
        r.save("midturn-after-queued-compact", screen)
        report["说完之后排队那一行撤了"] = not any("排队中" in line for line in screen)

        # ── 二、Detach：/help 这种长文，执行完还能挂回来接着看 ──
        before = turn_finished()
        start_turn(master, sink, "第二句")
        os.write(master, b"/help")
        h.drain_until(master, sink, "/help", 5.0)
        report["敲 /help 那一刻回合还在跑"] = turn_running(master, sink)
        os.write(master, b"\r")
        h.drain(master, 2.5, sink)
        screen = h.render(bytes(sink))
        r.save("midturn-help", screen)
        # 判据取**帮助正文里的按键表**：命令清单在长回合里会被顶出视口，
        # 而按键表就贴在帮助的末尾，跟正文同屏（实测 09-20）。
        report["回合中 /help 真的执行了"] = any(
            "Esc Esc" in line or "Ctrl+D" in line for line in screen
        )
        # 挂回来：这一轮要照常跑完（库里多一条完成的轮）。
        deadline = time.time() + 120
        while time.time() < deadline and turn_finished() <= before:
            h.drain(master, 0.3, sink)
        report["_完成的轮数"] = [before, turn_finished()]
        report["分离之后这一轮照样跑完"] = turn_finished() > before
        screen = h.render(bytes(sink))
        r.save("midturn-after-help", screen)
        # 挂回来不该把已经看过的那半截再来一遍：正文里那句标志话只该有一份
        # 连续的块，不该出现两段一模一样的开头。
        joined = "\n".join(screen)
        report["挂回来没把看过的再放一遍"] = joined.count("第二句") <= 2

        # ── 三、Detach + 换会话：/new 把这一轮丢到后台 ──
        sessions_before = len(lane_sessions())
        finished_before = turn_finished()
        start_turn(master, sink, "第三句")
        os.write(master, b"/new")
        h.drain_until(master, sink, "/new", 5.0)
        report["敲 /new 那一刻回合还在跑"] = turn_running(master, sink)
        os.write(master, b"\r")
        h.drain(master, 3.0, sink)
        screen = h.render(bytes(sink))
        r.save("midturn-new", screen)
        report["_新建前后的会话数"] = [sessions_before, len(lane_sessions())]
        report["回合中 /new 真的开了新会话"] = len(lane_sessions()) > sessions_before
        report["/new 之后屏幕是空会话"] = not any("第三句" in line for line in screen)
        # 被丢到后台的那一轮照样跑完
        deadline = time.time() + 120
        while time.time() < deadline and turn_finished() <= finished_before:
            h.drain(master, 0.3, sink)
        report["丢到后台的那一轮照样跑完"] = turn_finished() > finished_before

        # ── 四、Inline：/goal 仍然就地执行，不打断跟随 ──
        start_turn(master, sink, "第四句")
        os.write(master, "/goal 随便定个目标".encode())
        h.drain_until(master, sink, "随便定个目标", 5.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=0.8, timeout=20)
        screen = h.render(bytes(sink))
        r.save("midturn-goal", screen)
        report["/goal 仍然就地执行（输入被吃掉）"] = not any(
            "/goal 随便定个目标" in line for line in screen
        )
        # 「没打断跟随」的直接证据是**正文还在往下流**，不是屏幕上还看得见
        # 那句用户消息——长回合里它早就被顶出视口了（09-20 实测）。
        report["/goal 之后跟随没断（正文还在流）"] = turn_running(master, sink)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{sum(1 for v in checks.values() if v)}/{len(checks)} passed")
    for name, value in report.items():
        if name.startswith("_"):
            print(f"   {name[1:]}: {value}")
    print("产物：", h.OUT)
