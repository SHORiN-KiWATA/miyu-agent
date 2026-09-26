#!/usr/bin/env python3
"""`/session` 菜单里 Ctrl+D **当场删**，没有 y/N（用户 09-20 拍板）。

原来按一下 Ctrl+D 弹一行「删除「xx」？y/N」再等一个键。现在一按就删，删掉的那
一行从列表里消失就是回执；光标停在原位（下面的行顶上来），可以连着删。

判五件事：

- 按一下就少一条，屏幕上**不出现** y/N；
- 删的是**光标那一条**，不是别的；
- 连着按能连着删（光标不会跳回顶上）；
- 删掉**当前会话**也不要确认，之后 REPL 还能用（落到另一条会话上）；面板留着、
  开在兜底会话上，可以接着删（09-25：原来删完当前会话面板就收了）；
- Esc 退出菜单时不会顺手删掉什么。

跑之前给它私有端口和沙箱家，别跟别的走查抢：

    cargo build
    MIYU_HOME=/tmp/miyu-sessdel/home MIYU_TUI_PORT=18465 STUB_PORT=18466 \\
      MIYU_TUI_RUNTIME=/tmp/mx-sessdel OUT=~/.cache/miyu-sessdel \\
      python3 testkit/tui/session_delete.py
"""

import os
import sqlite3
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

STUB = {"STUB_CHUNK_SLEEP": "0.01", "STUB_REPLY": "好了"}


def sessions():
    """库里这条人格下的会话（名字 → id），按建的顺序。"""
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return []
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        return connection.execute(
            "SELECT session_id, name FROM sessions"
            " WHERE kind = 'user' AND session_id != 'default' ORDER BY rowid"
        ).fetchall()
    finally:
        connection.close()


def pointer(persona="default"):
    """这条车道的 REPL 会话指针。"""
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return None
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        row = connection.execute(
            "SELECT value FROM app_state WHERE key = ?",
            (f"repl_session_persona:{persona}",),
        ).fetchone()
    finally:
        connection.close()
    return row[0] if row else None


def command(master, sink, text, quiet=0.8):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=25)
    return h.render(bytes(sink))


def ask(master, sink, text):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=1.2, timeout=60)
    return h.render(bytes(sink))


def open_picker(master, sink):
    os.write(master, b"/session")
    h.drain_until(master, sink, "/session", 5.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=0.7, timeout=25)
    return h.render(bytes(sink))


# 会话有几条**一律查库**。从屏幕上数行两次都数错：全屏面板的行自带 `┃`，
# 按「有没有 ┃」排 footer 会把会话行一起排掉（09-20，和 new_session.py 同一个
# 坑）。删没删掉是库里的事实，就去库里问；屏幕只用来判「有没有弹 y/N」。


def selected_row(screen):
    return next((line.strip() for line in screen if "›" in line and " · " in line), "")


def ctrl_d(master, sink):
    os.write(master, b"\x04")
    h.settle(master, sink, quiet=0.7, timeout=25)
    return h.render(bytes(sink))


def esc(master, sink):
    os.write(master, b"\x1b")
    h.settle(master, sink, quiet=0.6, timeout=15)
    return h.render(bytes(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        # 攒四条有名字的会话，好认出删掉的是哪一条。
        #
        # 先说一句让**启动那条**不空：09-20 起 `/new` 在空会话里只改名不新建，
        # 不先说话的话第一句 `/new 甲会话` 会落在启动那条空会话上，只攒得出
        # 三条（准备段失效，后面的计数全偏）。
        ask(master, sink, "开场白")
        for name in ("甲会话", "乙会话", "丙会话"):
            command(master, sink, f"/new {name}")
            ask(master, sink, f"这是{name}")
        before = sessions()
        report["_开局的会话"] = [row[1] for row in before]
        report["攒够了会话"] = len(before) >= 4

        # ── 零、终端集成会话不该出现在 /session 里（用户 09-20） ──
        screen = open_picker(master, sink)
        r.save("sessdel-list", screen)
        report["列表里没有终端集成会话"] = not any(
            "终端集成会话" in line for line in screen
        )
        esc(master, sink)

        # ── 一、按一下就删，没有 y/N ──
        #
        # 先把光标从**当前会话**那一行移开：09-20 起删自己待着的那条会当场
        # 离开面板（另起一条新会话），和「连着删别的会话」是两条不同的路，
        # 混在一起测的是错的东西。初始选中的就是带 `*` 的当前会话。
        screen = open_picker(master, sink)
        os.write(master, b"j")
        h.settle(master, sink, quiet=0.4, timeout=10)
        screen = h.render(bytes(sink))
        count_before = len(sessions())
        target = selected_row(screen)
        report["_光标那一条"] = target
        screen = ctrl_d(master, sink)
        r.save("sessdel-after-one", screen)
        report["按一下就少一条"] = len(sessions()) == count_before - 1
        report["没弹 y/N"] = not any(
            "y/N" in line or "确认" in line for line in screen
        )
        # 删的是光标那一条：它的标题不该还在库里。
        title = target.split(" · ")[-1].strip() if " · " in target else ""
        report["_删掉的标题"] = title
        report["删的是光标那一条"] = bool(title) and all(
            row[1] != title for row in sessions()
        )

        # ── 二、连着删 ──
        screen = ctrl_d(master, sink)
        r.save("sessdel-after-two", screen)
        report["连着按能连着删"] = len(sessions()) == count_before - 2
        report["连删之后也没弹 y/N"] = not any("y/N" in line for line in screen)

        # ── 三、Esc 退出不会顺手删 ──
        count_now = len(sessions())
        esc(master, sink)
        open_picker(master, sink)
        esc(master, sink)
        report["Esc 退出没有多删"] = len(sessions()) == count_now

        # ── 四、删的是真的删了，不是只从屏幕上消失 ──
        after = sessions()
        report["_剩下的会话"] = [row[1] for row in after]
        report["库里真的删掉了"] = len(after) == len(before) - 2

        # ── 五、删掉**当前**会话也不确认，之后 REPL 还能用 ──
        command(master, sink, "/new 待删的当前会话")
        ask(master, sink, "这是当前会话")
        before_ids = {row[0] for row in sessions()}
        current_id = pointer()
        screen = open_picker(master, sink)
        # 当前会话在列表里带 `*`，也是初始选中的那条。
        report["_删前选中的"] = selected_row(screen)
        report["选中的就是当前会话"] = "*" in selected_row(screen)
        screen = ctrl_d(master, sink)
        r.save("sessdel-current", screen)
        report["删当前会话不弹 y/N"] = not any("y/N" in line for line in screen)
        # 总数**不会**少一条：删掉自己待着的那条之后会另起（或落到）一条会话。
        # 判的是「那条真的没了」+「现在待在别的会话上」。
        after_ids = {row[0] for row in sessions()}
        report["_删前后的会话 id"] = [len(before_ids), len(after_ids)]
        report["被删的那条真的没了"] = (
            current_id is not None and current_id not in after_ids
        )
        report["落到了另一条会话上"] = (
            pointer() is not None and pointer() != current_id
        )
        # 删掉自己待着的那条之后应该**当场离开**：屏幕上不该还留着被删会话的正文
        # （用户 09-20 实测）。面板留着、开在兜底会话上，可以接着删（09-25）。
        r.save("sessdel-current-after", screen)
        h.settle(master, sink, quiet=1.2, timeout=30)
        screen = h.render(bytes(sink))
        r.save("sessdel-current-after2", screen)
        report["删当前会话后面板还开着，可以接着删"] = any(
            "选择会话" in line or "Select session" in line for line in screen
        )
        report["删当前会话后看不见它的正文"] = not any(
            "这是当前会话" in line for line in screen
        )
        # 落到的不该是「终端集成会话」。
        report["没有落进终端集成会话"] = not any(
            "终端集成会话" in line for line in screen
        )
        # Esc 收掉面板，接着说话。
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.6, timeout=5)
        # 还能接着说话（落到另一条会话上，没把 REPL 弄死）。
        screen = ask(master, sink, "删完还能说话")
        r.save("sessdel-after-current", screen)
        report["删掉当前会话后还能接着说话"] = any(
            "好了" in line for line in screen
        )
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
