#!/usr/bin/env python3
"""第二十六轮走查：回合**还在跑**的时候点时间线、命令跑到一半 Ctrl+C。

`run.py` 那 37 项都是回合结束、收成 `Worked for …` 之后才点开的；这一轮用户报的
三条恰恰发生在回合中间：

- 跑着的命令点开要有**流式**输出（展开着的内容跟着输出长）；
- 已经跑完的「编辑文件」在模型开口之前就得点得开、点开是 diff；
- 命令跑到一半 Ctrl+C，它收成一步「已中断」，而不是漏出 inline 那套
  `$ 运行命令×1 运行中 / ↳ / │` 卡片。

    cargo build
    python3 testkit/tui/round26.py

复用 `run.py` 的沙箱与 PTY 辅助。产物在 ~/.cache/miyu-tui-smoke/round26-*.txt。

**这些 TUI 走查只能一个一个跑**：它们共用同一个 `MIYU_HOME`（`/tmp/miyu-tui-smoke/home`）
和同一个桩模型端口，起头还会 `rmtree` 那个家目录。并行跑的话两边互相掀桌子，红成一片
而代码一点问题都没有。
"""

import json
import os
import re
import select
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

BRAILLE = set("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")


def is_running_row(line, marker):
    stripped = line.lstrip()
    return bool(stripped) and stripped[0] in BRAILLE and marker in line


LAST = {"screen": None}


def wait_screen(master, sink, predicate, timeout):
    """读到屏幕满足 predicate 为止。返回满足时的那一屏（超时 None，最后一屏
    留在 LAST 里，好看清到底卡在哪）。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                return None
            if not chunk:
                return None
            sink.extend(chunk)
        screen = h.render(bytes(sink))
        LAST["screen"] = screen
        if predicate(screen):
            return screen
    return None


def start(stub_env, config_extra=None, *, direct=False):
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    h.EDIT_FILE.parent.mkdir(parents=True, exist_ok=True)
    if h.EDIT_FILE.exists():
        h.EDIT_FILE.unlink()
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    if config_extra:
        # 在桩配置上再盖几项（比如把压缩的尾巴预算压到几十个词元，好让几轮
        # 小对话也压得动）。
        path = h.HOME / "config" / "config.jsonc"
        config = json.loads(path.read_text(encoding="utf-8"))
        config.update(config_extra)
        path.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
    h.kill_stale_daemon()
    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), **stub_env),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
        raise RuntimeError("桩模型没起来")
    daemon = None
    if not direct:
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(h.OUT / "round26-daemon.log").open("a"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            raise RuntimeError("daemon 没起来")
    tui, master = h.spawn_tui()
    sink = bytearray()
    h.drain(master, 3.0, sink)
    return stub, daemon, tui, master, sink


def stop(*processes):
    for process in processes:
        if process is None:
            continue
        try:
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=5)
        except Exception:
            try:
                process.kill()
            except Exception:
                pass


def save(name, screen):
    (h.OUT / f"round26-{name}.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")


def scenario_live_clicks(report):
    """跑着的时候点：命令行流式展开、跑完的编辑步立刻能点开看 diff。"""
    stub, daemon, tui, master, sink = start({
        "STUB_REASONING": "1",
        "STUB_TOOL": "1",
        "STUB_EDIT": "1",
        "STUB_EDIT_PATH": str(h.EDIT_FILE),
        # 输出的字样不能原样出现在命令文本里，否则"展开里有没有输出"光靠命令
        # 那一行就满足了（printf 的格式串和参数分开写，拼出来的词才是输出）。
        "STUB_TOOL_COMMAND": (
            "printf 'out-%s\\n' one; sleep 3; printf 'out-%s\\n' two; sleep 3"
        ),
        # 编辑跑完之后模型要"想"够久，才来得及在它开口之前点开那一步。
        "STUB_REASONING_TEXT": "这段思考只是为了拖时间，好让人来得及点开上面那一步。" * 12,
        "STUB_CHUNK_SLEEP": "0.05",
    })
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        # 1. 命令跑起来：转轮行上有命令
        screen = wait_screen(
            master, sink,
            lambda s: any(is_running_row(line, "运行命令") for line in s),
            30.0,
        )
        report["r26_04_running_command_row"] = screen is not None
        if screen is None:
            return
        row = next(i for i, line in enumerate(screen) if is_running_row(line, "运行命令"))
        save("live-command", screen)
        # 1b. 不展开也有一行流式输出：抬头底下 `│ out-one`；转轮在左边距、logo 还在。
        screen = wait_screen(
            master, sink,
            lambda s: any(
                # 09-17 起跑着的那一行底下露的是**命令本身**（抬头给 title），
                # 不是输出——原断言找的是 `out-one`，那是改之前的样子。
                is_running_row(l, "运行命令")
                and i + 1 < len(s)
                and s[i + 1].startswith("  │")
                and "printf" in s[i + 1]
                for i, l in enumerate(s)
            ),
            8.0,
        )
        report["r26_04_live_output_line_under_row"] = screen is not None
        save("live-command-tail", screen or LAST["screen"] or [])
        running = next((l for l in (screen or LAST["screen"] or []) if is_running_row(l, "运行命令")), "")
        report["r26_01_spinner_in_margin_logo_kept"] = running.startswith(tuple(BRAILLE)) and " $ " in running[:6]
        # 2. 点开它：得有命令和已经吐出来的输出。流式期间屏幕静不下来（转轮
        #    每帧都在画），点击的等待要短。
        h.click(master, sink, 5, row, quiet=0.3, timeout=1.0)
        opened = h.render(bytes(sink))
        save("live-command-open", opened)
        # 正文贴着活动区往上长：展开之后整块上顶，行号全变，按整屏找。
        report["r26_04_open_shows_command"] = any("printf" in l for l in opened)
        report["r26_04_open_shows_first_output"] = any("out-one" in l for l in opened)
        # 3. 等第二行吐出来：展开着的内容要跟着长
        screen = wait_screen(
            master, sink,
            lambda s: any("out-two" in l for l in s),
            8.0,
        )
        report["r26_04_expansion_streams"] = screen is not None
        save("live-command-streamed", screen or LAST["screen"] or [])
        # 收回去：把手是展开之后 `$ 运行命令` 那一行
        head = next((i for i, l in enumerate(h.render(bytes(sink))) if l.strip().startswith("$ 运行命令")), None)
        if head is not None:
            h.click(master, sink, 5, head, quiet=0.3, timeout=1.0)
        # 4. 编辑那一步跑完、模型还在想：它已经换成静态图标，且屏上还有转轮行
        def edit_done_turn_running(s):
            edit = [l for l in s if "编辑文件" in l]
            return bool(edit) and not any(is_running_row(l, "编辑文件") for l in edit) \
                and any(l.lstrip() and l.lstrip()[0] in BRAILLE for l in s)
        screen = wait_screen(master, sink, edit_done_turn_running, 40.0)
        report["r26_02_edit_done_while_running"] = screen is not None
        if screen is None:
            save("live-edit-timeout", LAST["screen"] or [])
            return
        row = next(i for i, l in enumerate(screen) if "编辑文件" in l)
        save("live-edit", screen)
        # 命令跑完之后抬头底下还留着东西（连线穿过），不用点开（用户：完成后保留
        # 区域）。上面已经把展开收回去了，这几行是尾巴不是展开。
        #
        # 09-17 起留的是**命令本身**,不是输出(抬头给 title,跑了什么比吐了什么
        # 要紧,输出退到点开里)——原断言找的是 `out-two`,那是改之前的样子。
        report["r26_06_finished_command_keeps_tail"] = any(
            l.startswith("  │") and "printf" in l for l in screen
        )
        h.click(master, sink, 5, row, quiet=0.3, timeout=1.0)
        opened = h.render(bytes(sink))
        save("live-edit-open", opened)
        report["r26_02_edit_opens_to_diff_before_reply"] = any(
            "走查用的第一行" in l for l in opened
        ) and not any("走查的回复" in l for l in opened)
        # 5. 让它说完，收缩之后那一步还在（同一块 id，展开状态跟着走）
        h.drain_until(master, sink, "走查的回复", 40.0)
        h.settle(master, sink)
        final = h.render(bytes(sink))
        save("live-final", final)
        report["r26_02_reply_seen"] = any("走查的回复" in l for l in final)
    finally:
        stop(tui, daemon, stub)


def scenario_interrupt(report):
    """命令跑到一半 Ctrl+C。"""
    stub, daemon, tui, master, sink = start({
        "STUB_REASONING": "1",
        "STUB_TOOL": "1",
        "STUB_TOOL_COMMAND": "printf '开始了\\n'; sleep 40",
    })
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        screen = wait_screen(
            master, sink,
            lambda s: any(is_running_row(line, "运行命令") for line in s),
            30.0,
        )
        report["r26_03_running_command_row"] = screen is not None
        if screen is None:
            return
        # 等它的第一行输出到了再打断，打断前的输出得留在详情里
        wait_screen(master, sink, lambda s: "开始了" in "\n".join(s), 5.0)
        mark = len(sink)
        t0 = time.time()
        os.write(master, b"\x03")
        # 量延迟：转轮行什么时候消失、「已取消」什么时候出现。
        gone = wait_screen(
            master, sink,
            lambda s: not any(is_running_row(line, "运行命令") for line in s),
            20.0,
        )
        report["t_ms_until_running_row_gone"] = int((time.time() - t0) * 1000) if gone else None
        toast = wait_screen(master, sink, lambda s: any("已取消" in l for l in s), 20.0)
        report["t_ms_until_cancel_toast"] = int((time.time() - t0) * 1000) if toast else None
        h.settle(master, sink, quiet=0.6, timeout=20.0)
        after = h.render(bytes(sink))
        save("interrupt", after)
        text = "\n".join(after)
        report["r26_03_no_inline_card"] = "×1" not in text and "↳" not in text
        (h.OUT / "round26-interrupt-raw.bin").write_bytes(bytes(sink))
        # 收缩行：09-24 起按类写——跑过命令是 `› Ran 1 command · … · 1 err · 220ms`，没跑命令才是
        # `› Worked for … · 1 tool`（打断得快的话没有秒数，只剩计数）。
        head = max(
            (i for i, l in enumerate(after) if "›" in l and any(word in l for word in ("command", "tool", "edit"))),
            default=None,
        )
        report["r26_03_timeline_folded"] = head is not None
        if head is None:
            return
        h.click(master, sink, 3, head)
        opened = h.render(bytes(sink))
        save("interrupt-open", opened)
        step = next((i for i, l in enumerate(opened) if "运行命令" in l and "已中断" in l), None)
        report["r26_03_step_says_interrupted"] = step is not None
        raw = bytes(sink)[mark:].decode("utf-8", "replace")
        report["r26_03_step_is_red"] = bool(re.search(r"\x1b\[31m[^\n]*运行命令[^\n]*已中断", raw))
        if step is not None:
            h.click(master, sink, 5, step)
            deep = h.render(bytes(sink))
            save("interrupt-deep", deep)
            report["r26_03_detail_keeps_output"] = any("开始了" in l for l in deep)
        # rebase 到 sandbox 提交之上：`/sandbox` 在全屏里要能用（没绑时说一声）。
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        os.write(master, "/sandbox".encode())
        h.drain_until(master, sink, "/sandbox", 3.0)
        os.write(master, b"\r")
        shown = wait_screen(master, sink, lambda s: any("沙盒" in l for l in s), 10.0)
        report["r26_sandbox_command_answers"] = shown is not None
        save("sandbox", shown or LAST["screen"] or [])
    finally:
        stop(tui, daemon, stub)


ROW_SECS = re.compile(r"运行命令 · ([0-9.]+)s")


def scenario_panel_spinner(report):
    """点跑着的子代理那一行：切进它的会话（会话项目第 3 段；原来开一块浮层，09-25 退役），
    里面正在跑的那条命令左边距上的转轮真的在转、秒数真的在走（用户实测过浮层上的转轮
    不动、`准备执行 · 0.0s` 不动——同一件事在子会话里照样要成立）。"""
    stub, daemon, tui, master, sink = start({
        "STUB_REASONING": "1",
        "STUB_SUBAGENT": "1",
        # 内层那条命令要慢：只有它还跑着的时候才有东西可转。
        "STUB_SUBAGENT_COMMAND": "sleep 8; printf 'SUBOUT\\n'",
        "STUB_REASONING_TEXT": "想一下。",
        "STUB_CHUNK_SLEEP": "0.05",
    })
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        screen = wait_screen(
            master, sink,
            lambda s: any(is_running_row(l, "走查子代理") for l in s),
            30.0,
        )
        report["r26_05_subagent_row_running"] = screen is not None
        if screen is None:
            save("panel-spinner-timeout", LAST["screen"] or [])
            return
        row = next(i for i, l in enumerate(screen) if is_running_row(l, "走查子代理"))
        h.click(master, sink, 5, row, quiet=0.3, timeout=1.0)

        def running_in_child(s):
            joined = "\n".join(s)
            return "子代理 ↳1" in joined and any(is_running_row(l, "运行命令") for l in s)

        first = wait_screen(master, sink, running_in_child, 15.0)
        report["r26_05_click_enters_the_child_with_its_running_row"] = first is not None
        if first is None:
            save("panel-spinner-open", LAST["screen"] or [])
            return
        save("panel-spinner-a", first)
        # 隔半秒再看一眼：转轮换了帧、那一行上的秒数涨了。
        h.drain(master, 0.5, sink)
        second = h.render(bytes(sink))
        save("panel-spinner-b", second)
        run_a = next((l for l in first if is_running_row(l, "运行命令")), "")
        run_b = next((l for l in second if is_running_row(l, "运行命令")), "")
        report["r26_05_running_row_spinner_animates"] = bool(run_a) and bool(run_b) and (
            run_a.lstrip()[0] != run_b.lstrip()[0]
        )
        secs_a = ROW_SECS.search(run_a)
        secs_b = ROW_SECS.search(run_b)
        report["r26_05_running_row_seconds_tick"] = bool(secs_a and secs_b) and float(
            secs_b.group(1)
        ) > float(secs_a.group(1))
        # 转轮在左边距、图标还在它右边。
        report["r26_05_running_row_has_logo"] = " $ " in run_a[:8] if run_a else False
        os.write(master, b"/back")
        h.drain_until(master, sink, "/back", 3.0)
        os.write(master, b"\r")
        h.drain_until(master, sink, "走查的回复", 40.0)
    finally:
        stop(tui, daemon, stub)


def scenario_links(report):
    """全屏里的链接：画屏时把 OSC 8 发出去（终端认得是链接），点一下由我们自己
    打开（全屏把鼠标捕获走了，终端自己那套失效）。用假的 xdg-open 收网址。"""
    fakebin = h.OUT / "fakebin"
    fakebin.mkdir(parents=True, exist_ok=True)
    log = h.OUT / "xdg-open.log"
    if log.exists():
        log.unlink()
    script = fakebin / "xdg-open"
    script.write_text(f"#!/bin/sh\nprintf '%s\\n' \"$1\" >> {log}\n", encoding="utf-8")
    script.chmod(0o755)
    saved_env = h.ENV
    h.ENV = dict(h.ENV, PATH=f"{fakebin}:{os.environ.get('PATH', '')}")
    stub, daemon, tui, master, sink = start({
        "STUB_REPLY": "See [Miyu docs](https://example.com/miyu-doc) and https://example.org/bare done.",
    })
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        screen = wait_screen(master, sink, lambda s: any("done." in l for l in s), 30.0)
        report["r26_07_reply_with_links_seen"] = screen is not None
        if screen is None:
            save("links-timeout", LAST["screen"] or [])
            return
        h.settle(master, sink, quiet=0.5, timeout=3.0)
        screen = h.render(bytes(sink))
        save("links", screen)
        raw = bytes(sink)
        report["r26_07_painter_emits_osc8"] = b"\x1b]8;;https://example.com/miyu-doc" in raw
        row = next((i for i, l in enumerate(screen) if "Miyu docs" in l), None)
        report["r26_07_markdown_link_title_shown"] = row is not None
        if row is not None:
            column = screen[row].index("Miyu docs") + 2
            h.click(master, sink, column, row, quiet=0.3, timeout=2.0)
            time.sleep(0.5)
            opened = log.read_text(encoding="utf-8") if log.exists() else ""
            report["r26_07_markdown_link_click_opens"] = "https://example.com/miyu-doc" in opened
        row = next((i for i, l in enumerate(screen) if "example.org/bare" in l), None)
        if row is not None:
            column = screen[row].index("example.org/bare") + 3
            h.click(master, sink, column, row, quiet=0.3, timeout=2.0)
            time.sleep(0.5)
            opened = log.read_text(encoding="utf-8") if log.exists() else ""
            report["r26_07_bare_url_click_opens"] = "https://example.org/bare" in opened
    finally:
        h.ENV = saved_env
        stop(tui, daemon, stub)


def scenario_new_session(report):
    """全屏里 `/new`：画布清空、切到空会话；`/session 1` 切回去能看到旧对话。"""
    stub, daemon, tui, master, sink = start({"STUB_CHUNK_SLEEP": "0.02"})
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        h.drain_until(master, sink, "走查的回复", 40.0)
        h.settle(master, sink)
        mark = len(sink)
        os.write(master, b"/new\r")
        # 新会话是空会话：全屏画的是大厅（星空每 40ms 一帧，等不到"静默"，settle 会
        # 跑满超时），「已切换到会话」是一条几秒就走的通知，所以到字节流里找。
        h.settle(master, sink, quiet=0.6, timeout=6.0)
        fresh = h.render(bytes(sink))
        save("new-session", fresh)
        report["r26_08_new_session_clears_screen"] = not any("走查的回复" in l for l in fresh)
        report["r26_08_new_session_says_switched"] = "已切换到会话".encode() in bytes(sink[mark:])
        # 切回旧会话：列表按最近活动排，/new 之后新会话多半是 1 号、旧的是 2 号，
        # 但不赌次序——2 号没回放出旧对话就再试 1 号。
        replayed = False
        for index in (b"2", b"1"):
            os.write(master, b"/session " + index + b"\r")
            h.settle(master, sink, quiet=0.6, timeout=20.0)
            back = h.render(bytes(sink))
            save("session-back", back)
            if any("走查的回复" in l for l in back):
                replayed = True
                break
        report["r26_08_switch_back_replays"] = replayed
    finally:
        stop(tui, daemon, stub)


def scenario_compact(report):
    """全屏里 `/compact`：等待时转轮行写着「正在压缩」，压完正文里有「上下文已压缩」
    一行（不是右上角的通知），它是一块，点开是摘要（桩模型的摘要就是它那句回复）。"""
    # 尾巴预算默认 16k 词元，桩模型几轮小对话全在尾巴里、没东西可折；压到几十个
    # 词元，三轮之后前面的就能折进摘要。
    stub, daemon, tui, master, sink = start(
        {"STUB_CHUNK_SLEEP": "0.02"},
        config_extra={"context": {"compact_tail_tokens": 40}},
    )
    try:
        # 只有一轮的会话 daemon 会说「没有可压缩的上下文」，多聊几轮再压。
        for round_index in range(1, 4):
            os.write(master, f"{h.PROMPT} 第{round_index}遍".encode())
            h.drain_until(master, sink, f"第{round_index}遍", 3.0)
            os.write(master, b"\r")
            wait_screen(
                master, sink,
                lambda s, n=round_index: sum(1 for l in s if "走查的回复" in l) >= n,
                40.0,
            )
            h.settle(master, sink)
        os.write(master, b"/compact\r")
        done = ("上下文已压缩", "没有可压缩的上下文")
        screen = wait_screen(
            master, sink,
            lambda s: any(any(mark in l for mark in done) for l in s),
            60.0,
        )
        report["r26_09_compact_writes_result_line"] = screen is not None
        if screen is None:
            save("compact-timeout", LAST["screen"] or [])
            return
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        screen = h.render(bytes(sink))
        save("compact", screen)
        # 结果行在正文里、退两格：不是右上角那种贴着右边的通知。「正在压缩」那句
        # 09-18 起只在等待时挂在转轮行上(compact_footer.py 验它),压完就撤。
        report["r26_09_compact_notice_in_body"] = any(
            l.startswith("  ") and any(mark in l for mark in done) for l in screen
        )
        row = next((i for i, l in enumerate(screen) if "上下文已压缩" in l and l.lstrip().startswith("›")), None)
        report["r26_09_compact_result_is_a_fold"] = row is not None
        if row is not None:
            before = sum(1 for l in screen if "走查的回复" in l)
            h.click(master, sink, 3, row, quiet=0.3, timeout=2.0)
            opened = h.render(bytes(sink))
            save("compact-open", opened)
            after = sum(1 for l in opened if "走查的回复" in l)
            report["r26_09_compact_summary_expands"] = after > before
    finally:
        stop(tui, daemon, stub)


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    for stale in h.OUT.glob("round26-*.txt"):
        stale.unlink()
    report = {}
    scenario_live_clicks(report)
    scenario_interrupt(report)
    scenario_panel_spinner(report)
    scenario_links(report)
    scenario_new_session(report)
    scenario_compact(report)
    (h.OUT / "round26-report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    failed = [key for key, value in report.items() if value is not True and not key.startswith("t_")]
    for key, value in report.items():
        mark = "·" if key.startswith("t_") else ("✓" if value is True else "✗")
        print(f"  {mark} {key}: {value}")
    print(f"产物：{h.OUT}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
