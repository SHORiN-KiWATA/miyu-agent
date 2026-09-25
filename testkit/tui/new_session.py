#!/usr/bin/env python3
"""`miyu` / `miyu dev` 启动开**新会话**（用户 09-20 拍板）真机走查。

判的四件事：

- 说过一句、退出、再开 → 屏幕上**看不到**上一句（是新会话，不是接着上次聊）；
- 但上键**调得出**上一句 —— 输入历史是按会话存的，开新会话后得从被换掉的那条
  接过来，否则每次敲 `miyu` 上键都是空的；
- 什么都没说就退出、再开 → **复用**那条空会话，会话列表不多一条（一条没说过话
  的会话和一条新建的会话用户分不出来，但会话列表分得出来）；
- `/dev` 与 `/normal` **保留**「切到那条车道最近用的会话」 —— 同一次拍板里明确
  要留（todolist 上写的是「也改成开新的」，用户在对话里推翻了那句）。

    cargo build
    python3 testkit/tui/new_session.py
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

STUB = {"STUB_CHUNK_SLEEP": "0.01", "STUB_REPLY": "知道了"}


def ask(master, sink, text):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=1.2, timeout=40)
    return h.render(bytes(sink))


def command(master, sink, text, quiet=0.8):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=20)
    return h.render(bytes(sink))


def quit_tui(tui, master, sink):
    """空输入按 Ctrl+D 退出（走正常收尾路径，指针才落得下去）。"""
    # 先 Esc：`/session` 面板里 Ctrl+D 是**删除会话**不是退出，面板万一还
    # 开着，一个 Ctrl+D 就把会话删了（走查自己把被测的东西改坏）。
    os.write(master, b"\x1b")
    time.sleep(0.4)
    try:
        h.drain(master, 0.4, sink)
    except OSError:
        pass
    os.write(master, b"\x04")
    for _ in range(60):
        if tui.poll() is not None:
            break
        time.sleep(0.25)
        try:
            h.drain(master, 0.2, sink)
        except OSError:
            break
    if tui.poll() is None:
        tui.terminate()
        try:
            tui.wait(timeout=5)
        except Exception:
            tui.kill()


OPENS = [0]


def reopen(sink):
    """再开一个 TUI（同一个 daemon、同一个家）。"""
    tui, master = h.spawn_tui()
    sink.clear()
    # 虚拟屏也得扔掉：只清 sink 不够，`render` 是增量喂的，长度没回退就不重建
    # （上一个 TUI 画的东西会留着，看上去像「新会话里还有上一句」）。
    h.reset_view()
    h.drain(master, 4.0, sink)
    OPENS[0] += 1
    (h.OUT / f"new-session-raw-open{OPENS[0]}.bin").write_bytes(bytes(sink))
    return tui, master


def lane_sessions(persona="default"):
    """这条车道现有几条会话 —— **直接查库**。

    本来是抠 `/session` 面板的行，两轮都数出 0：行格式、面板开没开、有没有被
    footer 那行「┃ 普通 · 模型」混进来，每一样都能让它骗人；而且面板开着时
    Ctrl+D 是**删除会话**，走查一不小心就把被测的东西改坏了。会话有几条是库
    里的事实，就去库里问。
    """
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return []
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            "SELECT session_id, name FROM sessions WHERE persona = ? AND kind = 'user'"
            "   AND session_id != 'default'",
            (persona,),
        ).fetchall()
    finally:
        connection.close()
    return rows


def lane_pointer(persona="default"):
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


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        # ── 一、第一次：说一句 ──
        screen = ask(master, sink, "第一次说的话")
        report["第一轮跑起来了"] = any("知道了" in line for line in screen)
        first_pointer = lane_pointer()
        report["_第一次的会话数"] = len(lane_sessions())
        quit_tui(tui, master, sink)

        # ── 二、再开：是新会话，但上键调得出上一句 ──
        tui, master = reopen(sink)
        screen = h.render(bytes(sink))
        r.save("new-session-second-open", screen)
        report["再开看不到上一句"] = not any("第一次说的话" in line for line in screen)
        # 上键：历史得从被换掉的那条会话接过来。
        os.write(master, b"\x1b[A")
        h.settle(master, sink, quiet=0.5, timeout=8)
        # 大厅的星空一直在动，读到的字节常常停在一帧中间：星空补丁先把输入框那一行擦了，
        # 同一帧后面才重画回来（09-25，miyu-fc）。截到最后一个整帧的结尾再看。
        raw = bytes(sink)
        end = raw.rfind(b"\x1b[?2026l")
        screen = h.render(raw[: end + len(b"\x1b[?2026l")] if end >= 0 else raw)
        r.save("new-session-history-up", screen)
        report["上键调得出上一句"] = any("第一次说的话" in line for line in screen)
        # 把输入框清干净，免得它被当成下一步的输入（Ctrl+U 在这个编辑器里
        # 不一定是清行，直接退格够了）。
        os.write(master, b"\x7f" * 40)
        h.settle(master, sink, quiet=0.4, timeout=6)

        second_pointer = lane_pointer()
        report["再开换了一条会话"] = (
            second_pointer is not None and second_pointer != first_pointer
        )

        # ── 三、什么都没说就退出、再开：复用那条空的 ──
        before = len(lane_sessions())
        quit_tui(tui, master, sink)
        tui, master = reopen(sink)
        after = len(lane_sessions())
        report["_空会话前后的条数"] = [before, after]
        report["空会话被复用没有多攒一条"] = after == before
        report["空会话被复用指针没动"] = lane_pointer() == second_pointer

        # ── 四、说一句让这条不空，退出再开：这次该多一条 ──
        ask(master, sink, "第二次说的话")
        quit_tui(tui, master, sink)
        tui, master = reopen(sink)
        grown = len(lane_sessions())
        report["_说过话之后的条数"] = grown
        report["说过话就真的开新会话"] = grown == after + 1

        # ── 五、/dev 与 /normal 保留「切到最近的」 ──
        #
        # 参照句必须是**这次启动**说的：上一段每次重开都换了新会话，「第二次
        # 说的话」留在上一条会话里，拿它当参照是判据自己搞错了现场。
        ask(master, sink, "普通车道这一句")
        screen = command(master, sink, "/dev")
        r.save("new-session-dev", screen)
        report["/dev 切得过去"] = any(
            "┃ 开发 ·" in line or "┃ dev ·" in line for line in screen
        )
        ask(master, sink, "开发车道说的话")
        screen = command(master, sink, "/normal")
        r.save("new-session-normal-again", screen)
        report["/normal 回到刚才那条普通会话（不是新开）"] = any(
            "普通车道这一句" in line for line in screen
        )
        screen = command(master, sink, "/dev")
        r.save("new-session-dev-again", screen)
        report["/dev 回到刚才那条开发会话（不是新开）"] = any(
            "开发车道说的话" in line for line in screen
        )

        # ── 六、/dev new 与 /normal new：开一条新的（用户 09-20 追加） ──
        #
        # 此刻在开发车道，那条会话里有「开发车道说的话」。
        dev_before = len(lane_sessions("dev"))
        screen = command(master, sink, "/dev new")
        r.save("new-session-dev-new", screen)
        report["_开发会话数（/dev new 前后）"] = [dev_before, len(lane_sessions("dev"))]
        report["/dev new 开了一条新的开发会话"] = (
            len(lane_sessions("dev")) == dev_before + 1
        )
        report["/dev new 之后看不见开发车道的旧话"] = not any(
            "开发车道说的话" in line for line in screen
        )
        # 新会话里说一句，好让下面的「回到最近的」有东西可认。
        ask(master, sink, "新开发会话这一句")
        screen = command(master, sink, "/normal")
        normal_before = len(lane_sessions())
        screen = command(master, sink, "/normal new")
        r.save("new-session-normal-new", screen)
        report["_普通会话数（/normal new 前后）"] = [normal_before, len(lane_sessions())]
        report["/normal new 开了一条新的普通会话"] = (
            len(lane_sessions()) == normal_before + 1
        )
        report["/normal new 之后看不见普通车道的旧话"] = not any(
            "普通车道这一句" in line for line in screen
        )
        # 不带参数的仍然是「回到最近的」：回开发车道该看见刚才新说的那句。
        screen = command(master, sink, "/dev")
        r.save("new-session-dev-plain-after-new", screen)
        report["不带参数的 /dev 仍然回到最近的"] = any(
            "新开发会话这一句" in line for line in screen
        )
        # 乱给参数要说一句，不能当成 new 默默开新会话。
        before_junk = len(lane_sessions("dev"))
        screen = command(master, sink, "/dev 乱七八糟")
        r.save("new-session-dev-junk", screen)
        report["乱给参数会说一句只认 new"] = any(
            "只认 new" in line or "only `new`" in line for line in screen
        )
        report["乱给参数没有偷偷开新会话"] = len(lane_sessions("dev")) == before_junk

        # ── 七、/new 在**空会话**里不再新建（用户 09-20 拍板） ──
        #
        # `/new` 原来是唯一会攒空会话的口子：连敲两次不说话就多两条。现在规则
        # 和启动、`/dev new` 统一——空会话永远只有一条。
        #
        # 先切回**普通车道**：上一段结束时人在开发车道，而下面数的是普通车道，
        # 不切的话数的是另一条车道，断言全会骗人（09-20 实测，四条一起红）。
        command(master, sink, "/normal")
        command(master, sink, "/new")  # 造一条空的
        empty_before = len(lane_sessions())
        # 浮层提示只活 2.2 秒，而 `command()` 在大厅里要等「安静 0.8 秒」——
        # 星空一直在动，它会一路等满超时，提示早散了。敲完就采。
        #
        # 采要在它活着的那段里**轮询**，不能读满 1.2 秒只看一眼：大厅每一帧都把
        # 提示擦掉、紧接着再补上（老毛病，09-23 量过），只看一眼就是赌停在哪一拍
        # （09-23 实测：6 次里看漏 2 次）。
        os.write(master, b"/new")
        h.drain_until(master, sink, "/new", 5.0)
        os.write(master, b"\r")
        said = h.drain_until(master, sink, "已经是一条新会话", 2.0) or h.drain_until(
            master, sink, "already on a new session", 0.2
        )
        screen = h.render(bytes(sink))
        r.save("new-session-new-on-empty", screen)
        report["_空会话里敲 /new 前后的条数"] = [empty_before, len(lane_sessions())]
        report["空会话里敲 /new 不再多一条"] = len(lane_sessions()) == empty_before
        report["空会话里敲 /new 会说一句"] = said
        # 带名字就把当前这条改名，不新建。
        command(master, sink, "/new 改个名字")
        report["带名字也不新建"] = len(lane_sessions()) == empty_before
        report["带名字把当前这条改名了"] = any(
            row[1] == "改个名字" for row in lane_sessions()
        )
        # 说一句让它不空，这时 /new 该真的新建。
        ask(master, sink, "让这条不空")
        before_real = len(lane_sessions())
        command(master, sink, "/new")
        report["非空会话里敲 /new 照样新建"] = len(lane_sessions()) == before_real + 1
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
