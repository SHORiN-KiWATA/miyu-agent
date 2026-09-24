#!/usr/bin/env python3
"""大厅之后的两条走查：第一条消息落在屏幕**顶部**；`/reset` 回大厅**不淡入**。

复用 `testkit/tui/run.py` 的沙箱、桩模型与 PTY（自己的家目录与端口，不碰 8300）。

    cargo build
    python3 testkit/oobe/top_probe.py

判据：
  1. 大厅可见（模式行「Tab 切换」在屏上）；
  2. 发一句话、等桩模型回完：那句话所在行 ≤ 第 3 行(卡片上方留一行空)，输入框仍钉在屏底；
  3. `/reset` 之后大厅回来：**第一帧**提示行 `/config` 的前景色就等于 1.5s 后
     的稳定色（淡入会从底色 INK 往上爬，第一帧是暗的）。启动那一次作对照：
     第一帧应比稳定色暗（还在淡入）。
  4. 大厅里 Ctrl+C 的「要退出请按 Ctrl+D」在输入框底下，不在左下角；
  5. reset 之后再发一句，仍在屏顶（旧对话与「已清空」提示都不该垫在上面）；
  6. `/config` 进设置再 Esc 出来：第一次亮光标发生在同步块里，光标落在输入框。
产物在 ~/.cache/miyu-oobe/top/。
"""
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

os.environ.setdefault("MIYU_HOME", "/tmp/miyu-oobe-top/home")
os.environ.setdefault("MIYU_TUI_RUNTIME", "/tmp/mx-oobe-top")
os.environ.setdefault("MIYU_TUI_PORT", "18435")
os.environ.setdefault("STUB_PORT", "18498")
os.environ.setdefault("OUT", str(Path.home() / ".cache" / "miyu-oobe" / "top"))

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tui"))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

BAR = "┃"
PROMPT = "走查一句"


def cells(y):
    """一行的 (列, 字符) 列表，宽字符的续格（空串）跳过。"""
    line = h._VIEW["screen"].buffer[y]
    out = []
    for x in range(h.COLS):
        data = line[x].data if x in line else " "
        if data == "":
            continue
        out.append((x, data))
    return out


def lobby_ready(screen):
    return any("Tab" in line and "切换" in line for line in screen)


def hint_color():
    """提示行里 `/config` 第一个字的前景色（pyte 记成 'rrggbb' 或 'default'）。"""
    scr = h._VIEW["screen"]
    for y in range(h.ROWS):
        row = cells(y)
        text = "".join(ch for _, ch in row)
        idx = text.find("/config")
        if idx >= 0:
            x = row[idx][0]
            return scr.buffer[y][x].fg
    return None


def first_row_with(screen, needle):
    return next((i for i, line in enumerate(screen) if needle in line), None)


def save(name, screen):
    (h.OUT / f"top-{name}.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    h.OUT.mkdir(parents=True, exist_ok=True)
    for stale in h.OUT.glob("top-*.txt"):
        stale.unlink()
    report = {}
    # 不用 r.start()：它进屏后先等 3s，启动那次淡入就抓不到了。
    if h.HOME.exists():
        import shutil
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()
    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = tui = None
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(h.OUT / "top-daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2
        tui, master = h.spawn_tui()
        sink = bytearray()

        # 1. 大厅（启动那次淡入作对照）
        screen = r.wait_screen(master, sink, lobby_ready, 15)
        report["lobby"] = screen is not None
        if screen is None:
            save("lobby-missing", r.LAST["screen"] or [])
            return 1
        # 提示行比模式行晚落在流里，第一帧可能还没有它：最多再等 0.3s（淡入要 1s）。
        boot_first = hint_color()
        for _ in range(6):
            if boot_first is not None:
                break
            h.drain(master, 0.05, sink)
            h.render(bytes(sink))
            boot_first = hint_color()
        h.drain(master, 1.5, sink)
        screen = h.render(bytes(sink))
        settled = hint_color()
        save("lobby", screen)
        report["t_boot_first_frame_color"] = boot_first
        report["t_settled_color"] = settled
        report["boot_fades_in"] = boot_first is not None and boot_first != settled

        # 2. 第一条消息 → 顶部
        os.write(master, PROMPT.encode())
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        screen = r.wait_screen(
            master, sink,
            lambda s: any(h.is_fold_summary(line) or "收到" in line for line in s), 20,
        )
        report["reply_arrived"] = screen is not None
        screen = screen or r.LAST["screen"] or []
        save("first-message", screen)
        row = first_row_with(screen, PROMPT)
        report["t_prompt_row"] = row
        # 缓冲区第一行是消息卡上方那一行留白,卡片本身从第 2 行起(┃ / ┃ 文字 / ┃)。
        report["prompt_at_top"] = row is not None and row <= 2
        bar_rows = [i for i, line in enumerate(screen) if line.startswith(BAR)]
        report["input_pinned_bottom"] = bool(bar_rows) and max(bar_rows) >= h.ROWS - 6
        report["no_lobby_while_chatting"] = not lobby_ready(screen)

        # 3. /reset → 大厅回来，第一帧就是稳定色
        os.write(master, b"/reset")
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lobby_ready, 10)
        report["lobby_back_after_reset"] = screen is not None
        reset_first = hint_color()
        h.drain(master, 1.5, sink)
        screen = h.render(bytes(sink))
        save("lobby-after-reset", screen)
        reset_settled = hint_color()
        report["t_reset_first_frame_color"] = reset_first
        report["t_reset_settled_color"] = reset_settled
        report["reset_no_fade"] = (
            reset_first is not None and reset_first == reset_settled == settled
        )
        report["prompt_gone_after_reset"] = first_row_with(screen, PROMPT) is None

        # 4. 大厅里 Ctrl+C：「要退出请按 Ctrl+D」该待在输入框底下，不是左下角
        os.write(master, b"\x03")
        screen = r.wait_screen(
            master, sink, lambda s: any("要退出请按" in line for line in s), 5
        )
        report["exit_hint_shown"] = screen is not None
        # 通知先于活动区落在流里，刚看到它时输入框可能还没画：等这一帧画完再判。
        h.drain(master, 0.3, sink)
        screen = h.render(bytes(sink))
        save("exit-hint", screen)
        hint_row = first_row_with(screen, "要退出请按")
        bar_rows = [i for i, line in enumerate(screen) if BAR in line]  # 大厅里竖条左边还有星
        report["t_exit_hint_row"] = hint_row
        report["t_lobby_bar_rows"] = bar_rows
        # 判据：提示框在输入框下方几行之内，且不贴左边（大厅输入框是居中的窄框）。
        report["exit_hint_under_input"] = (
            hint_row is not None
            and bool(bar_rows)
            and max(bar_rows) < hint_row <= max(bar_rows) + 6
            and len(screen[hint_row]) - len(screen[hint_row].lstrip()) > 4
        )
        h.drain(master, 2.5, sink)  # 让它自己消失

        # 5. reset 之后再发一句：还得在屏顶（用户实测就是这里掉到屏底）
        os.write(master, PROMPT.encode())
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        screen = r.wait_screen(
            master, sink,
            lambda s: any(h.is_fold_summary(line) or "收到" in line for line in s), 20,
        )
        report["second_reply_arrived"] = screen is not None
        screen = screen or r.LAST["screen"] or []
        save("second-message", screen)
        row = first_row_with(screen, PROMPT)
        report["t_second_prompt_row"] = row
        report["second_prompt_at_top"] = row is not None and row <= 2
        report["old_note_not_on_top"] = not any("已清空" in line for line in screen)

        # 6. 再 reset，进 /config 再 Esc 出来：光标不经过左上角
        os.write(master, b"/reset")
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lobby_ready, 10)
        report["lobby_back_again"] = screen is not None
        os.write(master, b"/config")
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lambda s: not lobby_ready(s), 10)
        report["config_opened"] = screen is not None
        save("config", screen or r.LAST["screen"] or [])
        # 让设置界面画完再打标：REPL 交出终端那一串（关括号粘贴、亮光标）也在流里，
        # 不等它过去就会把「进设置」的光标动作当成「出设置」的。
        h.drain(master, 1.0, sink)
        mark = len(sink)
        os.write(master, b"\x1b")
        screen = r.wait_screen(master, sink, lobby_ready, 10)
        report["lobby_back_after_config"] = screen is not None
        h.drain(master, 1.0, sink)
        screen = h.render(bytes(sink))
        save("after-config", screen)
        tail = bytes(sink[mark:])
        (h.OUT / "top-after-config.bin").write_bytes(tail)
        show = tail.find(b"\x1b[?25h")
        sync = tail.find(b"\x1b[?2026h")
        report["t_after_config_first_show_at"] = show
        report["t_after_config_first_sync_at"] = sync
        # 第一次亮光标必须发生在同步块里（光标此时已在输入框），而不是先亮在左上角。
        report["config_exit_cursor_in_sync_block"] = show != -1 and sync != -1 and sync < show
        cursor = h._VIEW["screen"].cursor
        report["t_after_config_cursor"] = (cursor.x, cursor.y)
        report["cursor_back_in_input"] = not (cursor.x == 0 and cursor.y == 0)

        os.write(master, b"\x04")
        h.drain(master, 1.0, sink)
    finally:
        r.stop(tui, daemon, stub)
    import json
    (h.OUT / "top-report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    failed = [k for k, v in report.items() if v is not True and not k.startswith("t_")]
    for key, value in report.items():
        mark = "·" if key.startswith("t_") else ("✓" if value is True else "✗")
        print(f"  {mark} {key}: {value}")
    print(f"产物：{h.OUT}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
