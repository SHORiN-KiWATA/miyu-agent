#!/usr/bin/env python3
"""只读模式与默认沙盒(09-23):Tab / Shift+Tab 随开随关,真二进制 + 桩模型走一遍。

三个场景,各起一个干净的沙箱家:

1. 大厅里 Shift+Tab 开只读 → 大厅模式行与状态行都亮出「只读」→ 发一句,桩模型
   跑一条写文件的命令 → 写不进去;模型这一轮收到 `<sandbox mode="read-only" …>`。
2. 回合进行中按 Tab(会话已经不空)→ 下一次工具调用就生效:同一轮两条命令,
   第一条在按键前已经起跑、照常写进去,第二条被拦;再按 Tab 关掉,`/sandbox` 查看
   不再说只读。
3. 全局「默认开启沙盒模式」、在 Miyu 家里打开 → 默认根是工作区:`/sandbox` 查看说
   「沙盒根(默认)」,往工作区里写得进去,往外写被拒;模型收到带工作区根的 `<sandbox …>`。
4. 同样开着默认沙盒、但在项目目录里打开(自动检测,用户 09-23)→ 默认根就是这个
   目录:写项目成功、写外面被拒。

场景 1 另外验「彻底只读」:往 /tmp 写也被拒(用户 09-23)。

写的目标放在 `~/.cache/miyu-readonly-walk`:沙箱家在 /tmp 下,而只读模式下 /tmp
照样可写,放那里验不出东西。

    cargo build
    python3 testkit/tui/readonly_toggle.py

产物(屏幕、桩模型收到的请求)在 ~/.cache/miyu-tui-smoke/readonly-*。和别的 TUI 走查
共用沙箱家与端口,**只能一个一个跑**(见 round26.py 模块头)。
"""

import json
import os
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import round26 as r  # noqa: E402
import run as h  # noqa: E402

TARGET = Path.home() / ".cache" / "miyu-readonly-walk"
REQUEST_LOG = h.OUT / "readonly-requests.jsonl"
TMP_PROBE = Path("/tmp/miyu-readonly-walk-tmp-probe.txt")
PROJECT = h.HOME.parent / "project"
BACKTAB = b"\x1b[Z"


def reset_target():
    if TARGET.exists():
        shutil.rmtree(TARGET)
    TARGET.mkdir(parents=True)
    if REQUEST_LOG.exists():
        REQUEST_LOG.unlink()
    TMP_PROBE.unlink(missing_ok=True)


def spawn_tui_in(cwd):
    """跟 run.spawn_tui 一样,只是换个启动目录(客户端目录决定默认沙盒的根)。"""
    import pty, fcntl, struct, subprocess, termios  # noqa: E401
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", h.ROWS, h.COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(h.BIN)], stdin=slave, stdout=slave, stderr=slave,
        env=h.ENV, cwd=str(cwd), preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    return process, master


def requests():
    if not REQUEST_LOG.exists():
        return []
    return [json.loads(line) for line in REQUEST_LOG.read_text(encoding="utf-8").splitlines() if line]


def messages_text(role=None):
    """桩模型收到过的所有消息正文(可按角色筛)。"""
    return "\n".join(
        message["content"]
        for request in requests()
        for message in request["messages"]
        if role is None or message["role"] == role
    )


def turn_done(screen):
    return any(h.is_fold_summary(line) or "好的,收到" in line for line in screen)


def footer_readonly(screen):
    # 只读开着时「只读」直接顶替模式那几个字(用户 09-23)。
    return any("┃ 只读 · " in line for line in screen)


def lobby_row(screen):
    return next((line for line in screen if "Shift+Tab" in line), "")


def key_hint_fg():
    """大厅那行 `Shift+Tab` 此刻的前景色（还没画出来是 None）。"""
    screen = h._VIEW["screen"]
    for y in range(screen.lines):
        row = screen.buffer[y]
        cells = [(row[x].data, x) for x in range(screen.columns) if row[x].data]
        text = "".join(data for data, _ in cells)
        if "Shift+Tab" in text:
            return row[cells[text.index("Shift+Tab")][1]].fg
    return None


def wait_lobby_faded_in(master, sink, timeout=6.0):
    """等大厅淡入走完：`Shift+Tab` 的颜色连着 0.3 秒不再变。

    淡入要到启动后三秒出头才走完（约 24 拍、每拍 40ms），`r.start` 只等了 3 秒。
    09-23 起按键不再排队等下一拍、当场就处理，按下去的那一刻常常还差最后一两
    拍——大厅那行比状态行暗一截，颜色判据按淡入走完的样子定，于是稳定地红。
    """
    deadline = time.time() + timeout
    last, since = None, time.time()
    while time.time() < deadline:
        h.drain(master, 0.1, sink)
        h.render(bytes(sink))
        now = key_hint_fg()
        if now != last:
            last, since = now, time.time()
        elif now is not None and time.time() - since >= 0.3:
            return True
    return False


def read_only_colors():
    """(状态行「只读」是否跟大厅 `Shift+Tab` 同色, 大厅「● 只读」是否跟它同色)。

    宽字符在 pyte 里占两格、第二格是空串:先把「第几个字 → 第几列」对好,不能拿
    拼起来的字符串下标直接当列号(前面有中文就错位)。
    """
    screen = h._VIEW["screen"]
    footer_fg, key_fg, lobby_same = None, None, False
    for y in range(screen.lines):
        row = screen.buffer[y]
        cells = [(row[x].data, x) for x in range(screen.columns) if row[x].data]
        text = "".join(data for data, _ in cells)

        def fg_at(index):
            return row[cells[index][1]].fg

        if "┃ 只读" in text:
            footer_fg = fg_at(text.index("┃ 只读") + 2)
        if "Shift+Tab" in text and "只读" in text:
            key_fg = fg_at(text.index("Shift+Tab"))
            word = fg_at(text.rindex("只读"))
            dot = fg_at(text.rindex("只读") - 2)
            lobby_same = key_fg == word == dot and key_fg != "default"
    return close_colors(footer_fg, key_fg), lobby_same


def close_colors(a, b, tolerance=16):
    """两个前景色算不算同一个颜色。大厅的字停在淡入的 23/24(比调色盘暗约 4%,
    普通模式的蓝也一样),状态行是满值——同一个金,差这一点。"""
    try:
        return all(abs(int(a[i:i + 2], 16) - int(b[i:i + 2], 16)) <= tolerance for i in (0, 2, 4))
    except (TypeError, ValueError):
        return a is not None and a == b


def send_prompt(master, sink, text=h.PROMPT):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")


def scenario_lobby_shift_tab(report):
    reset_target()
    stub, daemon, tui, master, sink = r.start({
        "STUB_TOOL": "1",
        "STUB_TOOL_COMMAND": (
            f"echo x > {TARGET}/s1.txt; echo rc=$?; echo t > {TMP_PROBE}; echo tmp_rc=$?"
        ),
        "STUB_REQUEST_LOG": str(REQUEST_LOG),
    })
    try:
        wait_lobby_faded_in(master, sink)
        screen = h.render(bytes(sink))
        before = lobby_row(screen)
        report["s1_lobby_row_offers_shift_tab"] = "只读" in before
        os.write(master, BACKTAB)
        screen = r.wait_screen(master, sink, footer_readonly, 5.0)
        r.save("readonly-lobby-on", screen or r.LAST["screen"] or [])
        report["s1_footer_shows_read_only"] = screen is not None
        # 切换不另打回执(用户 09-23:「只读已开：能读，哪儿都写不了」AI 味太重)。
        report["s1_no_toast_on_toggle"] = not any("只读已开" in l or "哪儿都写不了" in l for l in (screen or []))
        after = lobby_row(screen or [])
        report["s1_lobby_row_lights_up"] = bool(after) and after != before
        # 颜色(用户 09-23):「只读」是金色,状态行与大厅按同一个色深降级,都跟大厅
        # 那行提示按键的 `Shift+Tab` 同色。
        footer_same, lobby_same = read_only_colors()
        report["s1_footer_read_only_matches_key_hint"] = footer_same
        report["s1_lobby_read_only_matches_key_hint"] = lobby_same
        send_prompt(master, sink)
        screen = r.wait_screen(master, sink, turn_done, 30.0)
        r.save("readonly-lobby-turn", screen or r.LAST["screen"] or [])
        report["s1_turn_finished"] = screen is not None
        report["s1_write_blocked"] = not (TARGET / "s1.txt").exists()
        tool_output = messages_text("tool")
        report["s1_command_saw_the_refusal"] = "rc=1" in tool_output
        report["s1_tmp_is_read_only_too"] = not TMP_PROBE.exists() and "tmp_rc=1" in tool_output
        report["s1_model_told_read_only"] = '<sandbox mode="read-only"' in messages_text()
        report["s1_footer_still_read_only"] = footer_readonly(h.render(bytes(sink)))
    finally:
        r.stop(tui, daemon, stub)


def scenario_tab_mid_turn(report):
    reset_target()
    stub, daemon, tui, master, sink = r.start({
        "STUB_TOOL": "1",
        "STUB_TOOL_ROUNDS": "2",
        # 第几条命令就写 call<n>:起跑时先数目录里已有几个文件。
        "STUB_TOOL_COMMAND": (
            f"n=$(ls {TARGET} | wc -l); sleep 4; echo x > {TARGET}/call$n.txt; echo rc=$?"
        ),
        "STUB_REQUEST_LOG": str(REQUEST_LOG),
    })
    try:
        send_prompt(master, sink)
        screen = r.wait_screen(
            master, sink,
            lambda s: any(r.is_running_row(line, "运行命令") for line in s),
            20.0,
        )
        report["s2_first_command_running"] = screen is not None
        os.write(master, b"\t")
        screen = r.wait_screen(master, sink, footer_readonly, 5.0)
        r.save("readonly-midturn-on", screen or r.LAST["screen"] or [])
        report["s2_tab_turns_read_only_on_mid_turn"] = screen is not None
        screen = r.wait_screen(master, sink, turn_done, 40.0)
        r.save("readonly-midturn-done", screen or r.LAST["screen"] or [])
        report["s2_turn_finished"] = screen is not None
        report["s2_call_started_before_the_key_wrote"] = (TARGET / "call0.txt").exists()
        report["s2_next_call_blocked"] = not (TARGET / "call1.txt").exists()
        os.write(master, b"\t")
        screen = r.wait_screen(master, sink, lambda s: not footer_readonly(s), 5.0)
        report["s2_tab_turns_it_off"] = screen is not None
        send_prompt(master, sink, "/sandbox")
        screen = r.wait_screen(master, sink, lambda s: any("没有沙盒，读写不受限" in l for l in s), 5.0)
        r.save("readonly-midturn-view", screen or r.LAST["screen"] or [])
        report["s2_view_no_longer_read_only"] = screen is not None
    finally:
        r.stop(tui, daemon, stub)


def scenario_default_sandbox(report):
    reset_target()
    stub, daemon, tui, master, sink = r.start(
        {
            "STUB_TOOL": "1",
            "STUB_TOOL_COMMAND": (
                f"echo x > {TARGET}/out.txt; echo out_rc=$?; "
                'echo y > "$PWD/in.txt"; echo in_rc=$?; echo "pwd=$PWD"'
            ),
            "STUB_REQUEST_LOG": str(REQUEST_LOG),
        },
        # 写上当前版本号,迁移才不会把「默认开启」刷成关;这样一来引导完成标志
        # 也不会被迁移补上,得自己写。
        {"config_version": 6, "oobe_done": True, "tools": {"sandbox": {"default_enabled": True}}},
    )
    try:
        send_prompt(master, sink)
        screen = r.wait_screen(master, sink, turn_done, 30.0)
        r.save("readonly-default-turn", screen or r.LAST["screen"] or [])
        report["s3_turn_finished"] = screen is not None
        # 大厅里的回执会被星空盖住(既有行为),离开大厅之后再查看。
        send_prompt(master, sink, "/sandbox")
        screen = r.wait_screen(
            master, sink, lambda s: any("根目录" in l and "（默认）" in l for l in s), 8.0
        )
        r.save("readonly-default-view", screen or r.LAST["screen"] or [])
        report["s3_view_says_default_root"] = screen is not None
        # 表格给人看:模型那份英文摘要里的固定词不该漏到屏上(AGENTS §1.5.1)。
        report["s3_view_has_no_model_words"] = screen is not None and not any(
            "everything" in l or "read-only)" in l for l in screen
        )
        tool_output = messages_text("tool")
        report["s3_write_outside_blocked"] = (
            not (TARGET / "out.txt").exists() and "out_rc=1" in tool_output
        )
        written = list(h.HOME.glob("**/workspace/in.txt"))
        report["s3_write_inside_workspace_ok"] = bool(written) and "in_rc=0" in tool_output
        notice = next(
            (line for line in messages_text().splitlines() if line.startswith("<sandbox ")), ""
        )
        report["s3_model_told_workspace_root"] = "workspace" in notice and "mode=" not in notice
    finally:
        r.stop(tui, daemon, stub)


def scenario_project_directory(report):
    reset_target()
    if PROJECT.exists():
        shutil.rmtree(PROJECT)
    PROJECT.mkdir(parents=True)
    original = h.spawn_tui
    h.spawn_tui = lambda: spawn_tui_in(PROJECT)
    try:
        stub, daemon, tui, master, sink = r.start(
            {
                "STUB_TOOL": "1",
                "STUB_TOOL_COMMAND": (
                    'echo y > "$PWD/p.txt"; echo in_rc=$?; '
                    f"echo x > {TARGET}/out4.txt; echo out_rc=$?"
                ),
                "STUB_REQUEST_LOG": str(REQUEST_LOG),
            },
            {"config_version": 6, "oobe_done": True, "tools": {"sandbox": {"default_enabled": True}}},
        )
    finally:
        h.spawn_tui = original
    try:
        send_prompt(master, sink)
        screen = r.wait_screen(master, sink, turn_done, 30.0)
        r.save("readonly-project-turn", screen or r.LAST["screen"] or [])
        report["s4_turn_finished"] = screen is not None
        tool_output = messages_text("tool")
        report["s4_project_is_writable"] = (PROJECT / "p.txt").exists() and "in_rc=0" in tool_output
        report["s4_outside_still_blocked"] = (
            not (TARGET / "out4.txt").exists() and "out_rc=1" in tool_output
        )
        notice = next(
            (line for line in messages_text().splitlines() if line.startswith("<sandbox ")), ""
        )
        report["s4_model_told_project_root"] = str(PROJECT.resolve()) in notice
        send_prompt(master, sink, "/sandbox")
        screen = r.wait_screen(
            master, sink, lambda s: any(str(PROJECT.resolve()) in l for l in s), 8.0
        )
        r.save("readonly-project-view", screen or r.LAST["screen"] or [])
        report["s4_view_shows_project_root"] = screen is not None
    finally:
        r.stop(tui, daemon, stub)


def main():
    h.OUT.mkdir(parents=True, exist_ok=True)
    report = {}
    for scenario in (
        scenario_lobby_shift_tab,
        scenario_tab_mid_turn,
        scenario_default_sandbox,
        scenario_project_directory,
    ):
        try:
            scenario(report)
        except Exception as error:  # noqa: BLE001 走查要把后面的场景跑完
            report[f"{scenario.__name__}_crashed"] = False
            print(f"{scenario.__name__}: {error!r}")
        h.kill_stale_daemon()
        time.sleep(0.5)
    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed  ({h.OUT})")
    shutil.rmtree(TARGET, ignore_errors=True)
    raise SystemExit(0 if passed == len(report) else 1)


if __name__ == "__main__":
    main()
