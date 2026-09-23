#!/usr/bin/env python3
"""两处「按了要等一会儿才动」的时延量尺（09-23 用户：空会话切模式、AI 运行中打断都挺慢）。

- 大厅按 Tab 切普通/开发：按键 → 模式行换过来；换过来之后紧跟着敲一个字 → 回显。
- 回合中打断，三种时机（想的时候 / 说的时候 / 跑命令的时候）× 两种按法（Ctrl+C /
  连按两次 Esc）：按键 → 转轮消失、→ 「已取消」出现。

读屏是增量喂 pyte（不每次整段重放，否则读屏本身就几百毫秒），按键之后 5ms 一看。
每个场景换一个桩模型进程（daemon 不动，它每次请求现连）。

    MIYU_BIN=target/debug/miyu python3 testkit/tui/latency_probe.py [--rounds 3] [--json]
"""

import argparse
import json
import os
import re
import select
import shutil
import signal
import socket
import struct
import statistics
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

import pyte  # noqa: E402

# 模式行：`◉ 普通模式   ○ 开发模式`，英文界面是 `◉ normal   ○ dev`。
DEV_ON = re.compile(r"◉ (开发|dev)")
NORMAL_ON = re.compile(r"◉ (普通|normal)")
CANCEL_TOAST = ("已取消", "cancelled", "Cancelled")
LONG_THOUGHT = "先把这一段想清楚再动手，不然后面要返工。" * 120
LONG_REPLY = "这是一段很长的回复正文，用来在它说到一半的时候打断。\n" * 200
# 真模型常见的正文：分段、列表、行内代码混着来。
FAST_REPLY = (
    "## 第{n}节\n\n先说结论：这一段是用来量流式输出时打断有多快的正文，"
    "里面有 `行内代码`、**加粗** 和一个列表。\n\n- 第一条要点，写得稍微长一点，"
    "让它在窄一点的窗口里会折行。\n- 第二条要点。\n- 第三条要点。\n\n"
)
FAST_REPLY = "".join(FAST_REPLY.replace("{n}", str(index)) for index in range(80))


class Tty:
    """一个 TUI 进程的 PTY：增量喂 pyte。"""

    def __init__(self, master):
        self.master = master
        self.screen = pyte.Screen(h.COLS, h.ROWS)
        self.stream = pyte.ByteStream(self.screen)

    def pump(self, timeout):
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return False
        try:
            chunk = os.read(self.master, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        self.stream.feed(chunk)
        return True

    def text(self):
        try:
            return "\n".join(self.screen.display)
        except IndexError:
            # 读到写了一半的全角字（前半格被盖掉）：等下一块到了再看。
            return ""

    def settle(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            self.pump(0.01)

    def wait_since(self, started, predicate, timeout):
        """等到 `predicate(屏幕文字)` 成立；返回距 `started` 的毫秒数，超时 None。"""
        if predicate(self.text()):
            return round((time.time() - started) * 1000, 1)
        while time.time() - started < timeout:
            if self.pump(0.005) and predicate(self.text()):
                return round((time.time() - started) * 1000, 1)
        return None

    def send(self, data):
        os.write(self.master, data)


def start_stub(env):
    # 单个环境变量不能超过 128KB（execve 的 MAX_ARG_STRLEN）：长的先落文件，
    # 由桩模型进程自己读回环境里再跑。
    env = dict(env)
    spilled = {}
    for key, value in list(env.items()):
        if len(value.encode()) > 64 * 1024:
            path = h.OUT / f"latency-{key.lower()}.txt"
            path.write_text(value, encoding="utf-8")
            spilled[key] = str(path)
            del env[key]
    loader = (
        "import os, runpy, sys\n"
        f"for key, path in {spilled!r}.items():\n"
        "    os.environ[key] = open(path, encoding='utf-8').read()\n"
        f"runpy.run_path({str(h.SMOKE / 'stub_llm.py')!r}, run_name='__main__')\n"
    )
    stub = subprocess.Popen(
        [sys.executable, "-c", loader],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), **env),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
        raise RuntimeError("桩模型没起来")
    return stub


def stop(process):
    if process is None:
        return
    try:
        process.send_signal(signal.SIGTERM)
        process.wait(timeout=5)
    except Exception:
        process.kill()


WAVE = set("▁▂▃▄▅▆▇")


def major_faults(pid):
    """进程累计的主缺页次数（要从磁盘 / swap 读回来的那种）。"""
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(fields[9])
    except Exception:
        return None


def running(text):
    """回合还在跑：转轮字形（braille 点阵）或 footer 的声波还在。"""
    return any("⠀" <= ch <= "⣿" or ch in WAVE for ch in text)


def cold_tab(tty, idle_secs, pids):
    """闲置 `idle_secs` 秒（照常读屏）之后按一次 Tab：量时延和两边的主缺页。"""
    tty.settle(idle_secs)
    before = {name: major_faults(pid) for name, pid in pids.items()}
    started = time.time()
    tty.send(b"\t")
    switched = tty.wait_since(started, lambda text: bool(DEV_ON.search(text)), 20)
    tty.settle(0.3)
    after = {name: major_faults(pid) for name, pid in pids.items()}
    faults = {name: (after[name] - before[name]) if None not in (after[name], before[name]) else None
              for name in pids}
    started = time.time()
    tty.send(b"\t")
    back = tty.wait_since(started, lambda text: bool(NORMAL_ON.search(text)), 20)
    return {"idle_secs": idle_secs, "switched_ms": switched, "major_faults": faults, "hot_back_ms": back}


def ipc(command):
    """直接对沙箱 daemon 发一条 IPC（4 字节大端长度 + JSON），返回 (毫秒, 回帧)。"""
    # 运行目录按家目录哈希分：`<XDG_RUNTIME_DIR>/miyu-<哈希>/core.sock`，取最新那个。
    path = max(Path(h.RUNTIME).glob("miyu*/core.sock"), key=lambda sock: sock.stat().st_mtime)
    started = time.perf_counter()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.connect(str(path))
        body = json.dumps(dict(version=3, **command)).encode()
        sock.sendall(struct.pack(">I", len(body)) + body)
        head = b""
        while len(head) < 4:
            head += sock.recv(4 - len(head))
        (length,) = struct.unpack(">I", head)
        data = b""
        while len(data) < length:
            data += sock.recv(length - len(data))
    return round((time.perf_counter() - started) * 1000, 1), json.loads(data)


def longest_session():
    _, frame = ipc({"command": "list_sessions", "mode": None})
    sessions = frame.get("data", {}).get("sessions", [])
    best = max(sessions, key=lambda row: row.get("turn_count", 0), default=None)
    return best and best.get("session_id")


def prime_long_session(tty, turns, chars):
    """用桩模型灌 `turns` 轮、每轮 `chars` 字的回复，把上下文堆起来。

    每灌完一轮量一次 `GetSessionState`：上下文词元数 → daemon 答一次要多久。"""
    block = "长会话垫底的正文，一行一行地往上堆，用来把上下文撑到几十万词元。\n"
    reply = (block * (chars // len(block) + 1))[:chars]
    stub = start_stub({"STUB_REPLY": reply, "STUB_CHUNK_CHARS": "4000", "STUB_CHUNK_SLEEP": "0"})
    curve = []
    try:
        for index in range(turns):
            tty.settle(0.3)
            started = time.time()
            tty.send(f"垫底第 {index} 轮\r".encode())
            tty.wait_since(started, running, 20)
            tty.wait_since(started, lambda text: not running(text), 600)
            turn_ms = round((time.time() - started) * 1000)
            session_id = longest_session()
            elapsed, frame = ipc({"command": "get_session_state",
                                  "target": {"kind": "id", "id": session_id}, "cwd": None})
            state = frame.get("state", {}) if isinstance(frame, dict) else {}
            curve.append({"turns": index + 1, "context_tokens": state.get("context_tokens"),
                          "context_window": state.get("context_window"),
                          "get_session_state_ms": elapsed, "turn_ms": turn_ms})
    finally:
        stop(stub)
    return curve


def cold_interrupt(tty, idle_secs, pids):
    """闲置之后发一句、想到一半按 Ctrl+C：量时延和两边的主缺页。"""
    stub = start_stub(dict(SCENARIOS[0][1]))
    try:
        tty.settle(idle_secs)
        tty.send("冷打断测试\r".encode())
        tty.wait_since(time.time(), running, 15)
        tty.settle(2.0)
        before = {name: major_faults(pid) for name, pid in pids.items()}
        started = time.time()
        tty.send(b"\x03")
        stopped = tty.wait_since(started, lambda text: not running(text), 20)
        toast = tty.wait_since(started, lambda text: any(word in text for word in CANCEL_TOAST), 20)
        tty.settle(0.3)
        after = {name: major_faults(pid) for name, pid in pids.items()}
    finally:
        stop(stub)
    faults = {name: (after[name] - before[name]) if None not in (after[name], before[name]) else None
              for name in pids}
    return {"idle_secs": idle_secs, "spinner_gone_ms": stopped, "toast_ms": toast, "major_faults": faults}


def lobby_tab(tty, rounds):
    rows = []
    for _ in range(rounds):
        for target in (DEV_ON, NORMAL_ON):
            tty.settle(0.6)
            started = time.time()
            tty.send(b"\t")
            switched = tty.wait_since(started, lambda text: bool(target.search(text)), 10)
            # 换过来之后马上敲一个字：看输入框是不是立刻能用。
            started = time.time()
            tty.send(b"q")
            echoed = tty.wait_since(started, lambda text: "┃ q" in text, 5)
            # 退格删掉（Ctrl+U 在这个编辑器里不清行，留着的字会让下一次 Tab 变成补全）。
            tty.send(b"\x7f")
            rows.append({"to": "dev" if target is DEV_ON else "normal",
                          "switched_ms": switched, "echo_after_ms": echoed})
    return rows


# (名字, 桩模型环境, 每轮先开新会话)。「命令中」要新会话：桩模型只在会话里还没有
# 工具结果时才要命令。「首字前」「停顿中」是真模型常见、桩模型默认没有的两种空档：
# 请求发出去几秒没有任何回音；两块之间停好几秒。
SCENARIOS = [
    ("思考中", {"STUB_REASONING": "1", "STUB_REASONING_TEXT": LONG_THOUGHT, "STUB_CHUNK_SLEEP": "0.05"}, False),
    ("正文中", {"STUB_REPLY": LONG_REPLY, "STUB_CHUNK_SLEEP": "0.05"}, False),
    ("命令中", {"STUB_TOOL": "1", "STUB_TOOL_COMMAND": "sleep 30"}, True),
    ("首字前", {"STUB_RESPONSE_DELAY": "8"}, False),
    ("停顿中", {"STUB_REASONING": "1", "STUB_REASONING_TEXT": LONG_THOUGHT, "STUB_CHUNK_SLEEP": "4"}, False),
    # 真模型的流速：2 字一块、5ms 一块（约 400 字/秒），思考与正文都很长。
    ("快速思考中", {"STUB_REASONING": "1", "STUB_REASONING_TEXT": LONG_THOUGHT * 4,
                "STUB_CHUNK_CHARS": "2", "STUB_CHUNK_SLEEP": "0.005"}, False),
    ("快速正文中", {"STUB_REPLY": FAST_REPLY, "STUB_CHUNK_CHARS": "2", "STUB_CHUNK_SLEEP": "0.005"}, False),
]


def interrupts(tty, rounds, only, busy_secs):
    rows = []
    stub = None
    try:
        for name, env, fresh in SCENARIOS:
            if only and name not in only:
                continue
            stop(stub)
            stub = start_stub(env)
            for key_name, keys in (("Ctrl+C", [b"\x03"]), ("Esc Esc", [b"\x1b", b"\x1b"])):
                for index in range(rounds):
                    tty.settle(0.8)
                    if fresh:
                        tty.send(b"/new\r")
                        tty.settle(1.5)
                    tty.send(f"打断测试 {name} {key_name} {index}\r".encode())
                    # 等它真跑起来、再多跑一会儿（想/说/命令/等首字都已经开始）。
                    began = tty.wait_since(time.time(), running, 15)
                    tty.settle(busy_secs)
                    # 输出正忙的时候敲一个字：多久出现在输入框里。敲完退格删掉——
                    # 草稿不空时 Ctrl+C 先清草稿，不打断。
                    started = time.time()
                    tty.send(b"z")
                    echo = tty.wait_since(started, lambda text: "┃ z" in text, 15)
                    tty.send(b"\x7f")
                    tty.wait_since(time.time(), lambda text: "┃ z" not in text, 15)
                    was_running = running(tty.text())
                    for key in keys[:-1]:
                        tty.send(key)
                        tty.settle(0.15)
                    started = time.time()
                    tty.send(keys[-1])
                    stopped = tty.wait_since(started, lambda text: not running(text), 15)
                    toast = tty.wait_since(
                        started, lambda text: any(word in text for word in CANCEL_TOAST), 15
                    )
                    # 「已取消」之后马上敲一个字：输入框什么时候能用。
                    typed = time.time()
                    tty.send(b"w")
                    echo_after = tty.wait_since(typed, lambda text: "┃ w" in text, 30)
                    tty.send(b"\x7f")
                    rows.append({
                        "when": name,
                        "key": key_name,
                        "began_ms": began,
                        "echo_while_busy_ms": echo,
                        "was_running": was_running,
                        "spinner_gone_ms": stopped,
                        "toast_ms": toast,
                        "echo_after_cancel_ms": echo_after,
                    })
                    tty.settle(1.0)
    finally:
        stop(stub)
    return rows


def summary(values):
    got = [value for value in values if value is not None]
    if not got:
        return "—"
    return f"中位 {statistics.median(got):.0f}ms（{min(got):.0f}–{max(got):.0f}，{len(got)}/{len(values)}）"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--only", default="", help="只跑这些场景，逗号分隔（如 快速思考中,快速正文中）")
    parser.add_argument("--skip-tab", action="store_true", help="跳过大厅 Tab 那组")
    parser.add_argument("--cold-idle", type=float, default=0, help="先闲置这么多秒再按一次 Tab（冷页对照）")
    parser.add_argument("--cold-interrupt", type=float, default=0, help="先闲置这么多秒再发一句并打断（冷页对照）")
    parser.add_argument("--long-session", type=int, default=0, help="先灌这么多轮长回复（配合 --long-chars）")
    parser.add_argument("--long-chars", type=int, default=45000, help="垫底每轮回复多少字")
    parser.add_argument("--busy-secs", type=float, default=2.0, help="开始输出多少秒之后再打断")
    args = parser.parse_args()
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    if args.long_session:
        # 和用户一样开 1M 窗口，否则堆到十几万就先触发自动压缩了。
        path = h.HOME / "config" / "config.jsonc"
        config = json.loads(path.read_text(encoding="utf-8"))
        config["providers"][0]["model_context_window"] = {"stub-model": 1_000_000}
        config["context"] = {"default_context_window": 1_000_000,
                             "compact_at_ratio": 0.99, "compact_force_ratio": 0.995}
        path.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
    h.kill_stale_daemon()
    stub = start_stub({})
    daemon = subprocess.Popen(
        [str(h.BIN), "__daemon", "--port", str(h.PORT)],
        env=h.ENV,
        cwd=str(h.HOME),
        stdout=(h.OUT / "latency-daemon.log").open("w"),
        stderr=subprocess.STDOUT,
    )
    tui = None
    try:
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            raise RuntimeError("daemon 没起来")
        stop(stub)
        stub = None
        tui, master = h.spawn_tui()
        tty = Tty(master)
        if tty.wait_since(time.time(), lambda text: "A G E N T" in text, 20) is None:
            raise RuntimeError("大厅没出来")
        tty.settle(1.5)
        only = {name for name in args.only.split(",") if name}
        report = {}
        if args.long_session:
            report["long_curve"] = prime_long_session(tty, args.long_session, args.long_chars)
            session_id = longest_session()
            timings = []
            for _ in range(3):
                elapsed, frame = ipc({"command": "get_session_state",
                                      "target": {"kind": "id", "id": session_id}, "cwd": None})
                timings.append(elapsed)
            state = frame.get("state", {}) if isinstance(frame, dict) else {}
            report["long_session"] = {
                "session_id": session_id,
                "context_tokens": state.get("context_tokens"),
                "get_session_state_ms": timings,
            }
        if args.cold_idle:
            report["cold_tab"] = cold_tab(tty, args.cold_idle, {"tui": tui.pid, "daemon": daemon.pid})
        report["lobby_tab"] = [] if args.skip_tab else lobby_tab(tty, args.rounds)
        if args.cold_interrupt:
            report["cold_interrupt"] = cold_interrupt(tty, args.cold_interrupt, {"tui": tui.pid, "daemon": daemon.pid})
        report["interrupt"] = interrupts(tty, args.rounds, only, args.busy_secs)
    finally:
        stop(tui)
        stop(daemon)
        stop(stub)
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return
    tab = report["lobby_tab"]
    print("大厅 Tab 切模式")
    print(f"  按键 → 模式行换过来：{summary([row['switched_ms'] for row in tab])}")
    print(f"  换过来之后敲字 → 回显：{summary([row['echo_after_ms'] for row in tab])}")
    print("回合中打断")
    for name, _, _ in SCENARIOS:
        for key_name in ("Ctrl+C", "Esc Esc"):
            rows = [row for row in report["interrupt"] if row["when"] == name and row["key"] == key_name]
            if not rows:
                continue
            if not all(row["was_running"] for row in rows):
                print(f"  ! {name} {key_name}：有一轮按键时已经不在跑了")
            print(f"  {name} {key_name}：转轮消失 {summary([row['spinner_gone_ms'] for row in rows])}；"
                  f"「已取消」{summary([row['toast_ms'] for row in rows])}")


if __name__ == "__main__":
    main()
