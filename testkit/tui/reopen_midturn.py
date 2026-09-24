#!/usr/bin/env python3
"""回合跑着的时候关掉 TUI、再开一个，刚发的那句话会不会冒出第二条。

用户 09-21：「发送一个消息，AI 开始运行，然后我关闭 TUI，再重新打开 TUI 进那个
会话；AI 依旧在运行，这很好，但是刚刚发送的消息会重复一条进入排队中」。

    cargo build
    python3 testkit/tui/reopen_midturn.py

这些 TUI 走查只能一个一个跑（共用 MIYU_HOME 与桩模型端口）。
"""

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

MESSAGE = "reopen-probe 这句话只发一次"


def main():
    report = {}
    h.ENV["MIYU_LOG"] = "info"
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        # 回合要跑得够久：关掉再开的时候它得还在跑。
        "STUB_REASONING_TEXT": "慢慢想一想这件事该怎么办才稳妥。" * 150,
        "STUB_CHUNK_CHARS": "8",
        "STUB_CHUNK_SLEEP": "0.12",
    })
    second = None
    try:
        # 大厅还在画的时候按下的键会丢，先等它画完。
        h.drain_until(master, sink, "A G E N T", 15.0)
        h.drain(master, 1.0, sink)
        # 用户 09-21 的完整流程：开 miyu（自动就是新会话）→ 说一句话让 AI 跑着
        # → 关掉 TUI → 再开（又是新会话）→ `/session` 切回去。所以这句话是这条
        # 会话的**第一条**、是回合的主 prompt，不是排进正在跑的回合的跟进消息。
        os.write(master, MESSAGE.encode())
        h.drain_until(master, sink, MESSAGE, 5.0)
        os.write(master, b"\r")
        screen = r.wait_screen(
            master, sink,
            lambda s: any(
                line.lstrip()[:1] in set("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                for line in s if line.strip()
            ),
            30.0,
        )
        report["rm01_turn_is_running"] = screen is not None
        report["rm02_not_queued_before_leaving"] = not any(
            "排队中" in line or "Queued" in line for line in (screen or [])
        )
        (h.OUT / "reopen-before.txt").write_text(
            "\n".join(screen or r.LAST["screen"] or []) + "\n", encoding="utf-8"
        )
        # 拔掉终端，回合留给 daemon。
        os.close(master)
        r.stop(tui)
        # 开一个新的 TUI，先新建一个会话，再用 `/session` 切回去——用户就是
        # 这么回去的，而「切到当前会话」是 no-op，不走加载那条路。
        tui2, master2 = h.spawn_tui()
        second = tui2
        sink2 = bytearray()
        # 换了 TUI 进程：虚拟屏从零开始。不清的话 `render` 只在新流比旧流短时才重建，
        # 大厅四秒的动画一过旧流长度，新字节就接着画在第一个进程的残影上，下面找
        # 那句话可能找到的是残影（run.py `render`）。
        h.reset_view()
        h.drain(master2, 4.0, sink2)
        os.write(master2, b"\x15/new\r")
        h.drain(master2, 3.0, sink2)
        os.write(master2, b"\x15/session\r")
        screen = r.wait_screen(
            master2, sink2,
            lambda s: any("选择会话" in line or "Select session" in line for line in s),
            15.0,
        )
        report["rm03_session_picker_opens"] = screen is not None
        (h.OUT / "reopen-picker.txt").write_text(
            "\n".join(screen or r.LAST["screen"] or []) + "\n", encoding="utf-8"
        )
        # 往下挑一条不是当前的会话（当前那条排在最前）。
        os.write(master2, b"\x1b[B")
        h.drain(master2, 0.6, sink2)
        os.write(master2, b"\r")
        screen = r.wait_screen(
            master2, sink2, lambda s: any(MESSAGE in line for line in s), 30.0
        )
        report["rm04_message_is_back_on_screen"] = screen is not None
        screen = screen or r.LAST["screen"] or []
        (h.OUT / "reopen-screen.txt").write_text(
            "\n".join(screen) + "\n", encoding="utf-8"
        )
        # 它是这一轮正在处理的那句话：正文里一条就够，队列里**不该**再有一条
        # （用户 09-21：「还有一条一模一样的消息在排队里面」）。
        report["rm05_message_appears_once"] = (
            sum(1 for line in screen if MESSAGE in line) == 1
        )
        report["rm06_nothing_queued"] = not any(
            "排队中" in line or "Queued" in line for line in screen
        )
    finally:
        r.stop(second, daemon, stub)

    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
