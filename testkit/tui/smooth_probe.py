#!/usr/bin/env python3
"""TUI 顺滑度量尺：大厅空转、按键回显、悬浮、滚轮、斜杠命令各要多久。

用户 09-23：「我们的 TUI 会明显卡顿……会话长了之后鼠标悬浮的高亮有延迟……空会话
光标在频繁跳动」。这份量尺把「卡」拆成几个可复跑的数：

- **大厅空转**：空会话什么都不做时，每秒吐多少帧、多少字节、几次 write、吃多少 CPU；
- **按键回显**：敲一个字（和输入法一次上屏一串）到那一帧画完要多久；
- **悬浮**：指针换到另一块上到提亮那一帧画完要多久；一口气扫过全屏之后，
  最后一下落地到画面追上要多久（「拖尾」，就是用户说的「有种延迟」）；
- **回合中打字**：AI 正在输出时敲一个字、输入法上屏一串要多久出现（回合的输入泵
  和空闲时不是同一条路）；
- **滚轮**、**斜杠命令**（/help、/session 开面板）同理。

长会话用专用桩模型 `smooth_stub.py` 现造（每轮 `TOOLS:n` 次工具、一段思考、一段长
正文；`SLOW` 按真模型的节奏慢慢吐）。时间都从 PTY 主端量：写下事件
那一刻到对应的同步块收尾（`ESC[?2026l`）到达那一刻，含 Miyu 自己的处理，不含终端
（kitty/herdr）的解析与上屏。

    cargo build
    python3 testkit/tui/smooth_probe.py [--turns 40] [--strace]

`MIYU_BIN` 换二进制做 A/B。TUI 走查只能一个一个跑（共用端口与沙箱家目录）。
"""

import argparse
import json
import os
import re
import select
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

BEGIN = b"\x1b[?2026h"
END = b"\x1b[?2026l"


class Reader:
    """后台线程一直读 PTY：主线程发事件，这边记每一块字节到达的时刻。"""

    def __init__(self, master, sink):
        self.master = master
        self.sink = sink
        # (块结尾在 sink 里的偏移, 到达时刻)
        self.arrivals = []
        self.lock = threading.Lock()
        self.alive = True
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()

    def _loop(self):
        while self.alive:
            ready, _, _ = select.select([self.master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 262144)
            except OSError:
                break
            if not chunk:
                break
            now = time.perf_counter()
            with self.lock:
                self.sink.extend(chunk)
                self.arrivals.append((len(self.sink), now))

    def mark(self):
        with self.lock:
            return len(self.sink), time.perf_counter()

    def last_arrival(self):
        with self.lock:
            return self.arrivals[-1][1] if self.arrivals else 0.0

    def time_of(self, offset):
        """sink 里第 offset 个字节是什么时候到的。"""
        with self.lock:
            for end, at in self.arrivals:
                if end > offset:
                    return at
        return None

    def snapshot(self, since=0):
        with self.lock:
            return bytes(self.sink[since:])

    def wait_quiet(self, quiet, timeout):
        """等到连续 `quiet` 秒没有输出。返回最后一块到达的时刻。"""
        deadline = time.perf_counter() + timeout
        while time.perf_counter() < deadline:
            last = self.last_arrival()
            if time.perf_counter() - last >= quiet:
                return last
            time.sleep(0.01)
        return None

    def wait_for(self, needle, since, timeout):
        """等 sink[since:] 里出现 needle。返回 (needle 结尾的偏移, 到达时刻)。"""
        deadline = time.perf_counter() + timeout
        while time.perf_counter() < deadline:
            data = self.snapshot(since)
            pos = data.find(needle)
            if pos >= 0:
                offset = since + pos + len(needle) - 1
                return offset, self.time_of(offset)
            time.sleep(0.002)
        return None, None

    def stop(self):
        self.alive = False


def pct(values, q):
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round(q * (len(ordered) - 1))))
    return ordered[index]


def summary(values):
    if not values:
        return "—"
    return (
        f"p50 {pct(values, 0.5):.1f} / p90 {pct(values, 0.9):.1f} / "
        f"max {max(values):.1f} ms (n={len(values)})"
    )


def sync_trace_path():
    return Path("/tmp/miyu-sync-trace.log")


def reset_sync_trace():
    try:
        sync_trace_path().unlink()
    except FileNotFoundError:
        pass


def read_sync_trace():
    """进程内每个最外层同步块的时长（毫秒）。见 `tail/update.rs` 的 MIYU_SYNC_TRACE。

    每行 `微秒 [字节]`（09-23 起带上这一帧攒了多少字节；老二进制只有微秒）。
    """
    try:
        text = sync_trace_path().read_text()
    except FileNotFoundError:
        return []
    values = []
    for line in text.splitlines():
        fields = line.split()
        if fields and fields[0].isdigit():
            values.append(int(fields[0]) / 1000.0)
    return values


def idle_window(reader, pid, seconds):
    start_len, _ = reader.mark()
    cpu0 = h.cpu_ms(pid)
    reset_sync_trace()
    time.sleep(seconds)
    cpu1 = h.cpu_ms(pid)
    data = reader.snapshot(start_len)
    frames = data.count(BEGIN)
    blocks = read_sync_trace()
    return {
        "seconds": seconds,
        "bytes_per_s": round(len(data) / seconds),
        "frames_per_s": round(frames / seconds, 1),
        "bytes_per_frame": round(len(data) / frames) if frames else 0,
        "cpu_pct": round((cpu1 - cpu0) / (seconds * 10.0), 1) if cpu0 is not None else None,
        "sync_block_ms": summary(blocks),
        "cursor_moves_per_frame": round(data.count(b"\x1b[") / frames) if frames else 0,
    }


def type_latency(reader, master, text, timeout=5.0):
    """写下 text（一次写完，像输入法上屏）→ (回显出现, 回显那一帧收尾) 毫秒。"""
    since, _ = reader.mark()
    t0 = time.perf_counter()
    os.write(master, text.encode())
    offset, echo_at = reader.wait_for(text.encode(), since, timeout)
    if offset is None:
        return None, None
    end_offset, end_at = reader.wait_for(END, offset, timeout)
    if end_offset is None:
        return (echo_at - t0) * 1000, None
    return (echo_at - t0) * 1000, (end_at - t0) * 1000


def clear_draft(reader, master):
    # Ctrl+C：草稿非空时是「清草稿」（编辑器阶梯的第一级）。
    os.write(master, b"\x03")
    time.sleep(0.2)


CJK = "甲乙丙丁戊己庚辛壬癸子丑寅卯辰巳午未申酉戌亥"


def key_phase(reader, master, label, count=12, gap=0.12):
    echo, frame = [], []
    for index in range(count):
        char = CJK[index % len(CJK)]
        e, f = type_latency(reader, master, char)
        if e is not None:
            echo.append(e)
        if f is not None:
            frame.append(f)
        time.sleep(gap)
    clear_draft(reader, master)
    # 输入法一次上屏六个字：crossterm 拆成六个按键事件，每个都要重画一次。
    burst = []
    for index in range(5):
        text = "".join(CJK[(index * 6 + k) % len(CJK)] for k in range(6))
        _, f = type_latency(reader, master, text)
        if f is not None:
            burst.append(f)
        clear_draft(reader, master)
        time.sleep(gap)
    return {
        f"{label}_key_echo": summary(echo),
        f"{label}_key_frame": summary(frame),
        f"{label}_ime6_frame": summary(burst),
    }


def mouse_move(master, column, row):
    os.write(master, f"\x1b[<35;{column + 1};{row + 1}M".encode())


def find_rows(screen, predicate):
    return [index for index, line in enumerate(screen) if predicate(line)]


def hover_phase(reader, master, pid):
    screen = h.render(reader.snapshot())
    def is_block(line):
        return any(mark in line for mark in ("已思考", "thought", "运行命令")) or h.is_fold_summary(line)

    block_rows = find_rows(screen[: h.ROWS - 8], is_block)
    text_rows = find_rows(
        screen[: h.ROWS - 8],
        lambda line: line.strip().startswith("- 第") and not is_block(line),
    )
    result = {"hover_block_rows_on_screen": len(block_rows)}
    if not block_rows or not text_rows:
        result["hover"] = "屏上找不到可悬浮的块"
        return result
    column = 10
    single = []
    for index in range(12):
        row = block_rows[index % len(block_rows)] if index % 2 == 0 else text_rows[index % len(text_rows)]
        since, _ = reader.mark()
        t0 = time.perf_counter()
        mouse_move(master, column, row)
        end_offset, end_at = reader.wait_for(END, since, 2.0)
        if end_at is not None:
            single.append((end_at - t0) * 1000)
        reader.wait_quiet(0.15, 2.0)
    result["hover_single_frame"] = summary(single)
    # 一口气扫过全屏：8ms 一下（≈ 鼠标 125Hz），上下来回三趟。
    rows = list(range(1, h.ROWS - 8)) + list(range(h.ROWS - 9, 0, -1))
    rows = rows * 3
    reader.wait_quiet(0.3, 3.0)
    cpu0 = h.cpu_ms(pid)
    reset_sync_trace()
    since, t_start = reader.mark()
    for row in rows:
        mouse_move(master, column, row)
        time.sleep(0.008)
    t_sent = time.perf_counter()
    last = reader.wait_quiet(0.3, 20.0)
    cpu1 = h.cpu_ms(pid)
    blocks = read_sync_trace()
    data = reader.snapshot(since)
    result["hover_sweep_events"] = len(rows)
    result["hover_sweep_send_ms"] = round((t_sent - t_start) * 1000)
    # 最后一下事件发出去之后，画面还要多久才追上（0 = 发完时已经画完了）。
    result["hover_sweep_lag_ms"] = max(0, round((last - t_sent) * 1000)) if last else None
    result["hover_sweep_frames"] = data.count(BEGIN)
    result["hover_sweep_bytes"] = len(data)
    result["hover_sweep_cpu_ms"] = (cpu1 - cpu0) if cpu0 is not None else None
    result["hover_sweep_sync_block_ms"] = summary(blocks)
    return result


def wheel_phase(reader, master, pid):
    reader.wait_quiet(0.3, 3.0)
    cpu0 = h.cpu_ms(pid)
    since, t_start = reader.mark()
    for _ in range(15):
        os.write(master, f"\x1b[<64;20;10M".encode())
        time.sleep(0.015)
    for _ in range(15):
        os.write(master, f"\x1b[<65;20;10M".encode())
        time.sleep(0.015)
    t_sent = time.perf_counter()
    last = reader.wait_quiet(0.3, 20.0)
    cpu1 = h.cpu_ms(pid)
    data = reader.snapshot(since)
    return {
        "wheel_lag_ms": max(0, round((last - t_sent) * 1000)) if last else None,
        "wheel_frames": data.count(BEGIN),
        "wheel_bytes": len(data),
        "wheel_cpu_ms": (cpu1 - cpu0) if cpu0 is not None else None,
    }


def command_latency(reader, master, command, needle, close=b"\x1b"):
    reader.wait_quiet(0.3, 3.0)
    os.write(master, command.encode())
    time.sleep(0.15)
    since, _ = reader.mark()
    t0 = time.perf_counter()
    os.write(master, b"\r")
    offset, at = reader.wait_for(needle.encode(), since, 10.0)
    first = (at - t0) * 1000 if at else None
    last = reader.wait_quiet(0.3, 10.0)
    settled = (last - t0) * 1000 if last else None
    if close:
        os.write(master, close)
        reader.wait_quiet(0.3, 3.0)
    return first, settled


def build_session(reader, master, turns, tools):
    """每轮：`tools` 次「想一句 + 跑一条命令」，再一段带标题/列表/代码块的长正文。"""
    for index in range(turns):
        os.write(master, f"第{index}轮走查 TOOLS:{tools}".encode())
        time.sleep(0.05)
        os.write(master, b"\r")
        # 回合跑完：footer 转轮停了、输出静下来。
        time.sleep(0.3)
        reader.wait_quiet(0.6, 60.0)


def spawn_tui(prefix=()):
    """同 `run.spawn_tui`，但可以在前面套一层（strace 数 write 次数用）。"""
    import fcntl
    import pty
    import struct
    import termios

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", h.ROWS, h.COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [*prefix, str(h.BIN)], stdin=slave, stdout=slave, stderr=slave,
        env=h.ENV, cwd=str(h.HOME), preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    return process, master


def start(prefix=()):
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()
    stub = subprocess.Popen(
        [sys.executable, str(Path(__file__).resolve().parent / "smooth_stub.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
        raise RuntimeError("桩模型没起来")
    daemon = subprocess.Popen(
        [str(h.BIN), "__daemon", "--port", str(h.PORT)],
        env=h.ENV, cwd=str(h.HOME),
        stdout=(h.OUT / "smooth-daemon.log").open("w"), stderr=subprocess.STDOUT,
    )
    if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
        raise RuntimeError("daemon 没起来")
    tui, master = spawn_tui(prefix)
    return stub, daemon, tui, master, bytearray()


def stream_phase(reader, master, pid):
    """一段慢慢吐的回复（≈50 块/秒）：这期间每秒多少帧、多少字节，边流边打字多快回显。"""
    reader.wait_quiet(0.5, 10.0)
    os.write(master, "流式走查 SLOW".encode())
    time.sleep(0.05)
    since, _ = reader.mark()
    os.write(master, b"\r")
    time.sleep(1.5)
    cpu0 = h.cpu_ms(pid)
    reset_sync_trace()
    mark, t0 = reader.mark()
    time.sleep(3.0)
    mark2, t1 = reader.mark()
    cpu1 = h.cpu_ms(pid)
    blocks = read_sync_trace()
    data = reader.snapshot(mark)[: mark2 - mark]
    frames = data.count(BEGIN)
    result = {
        "stream_frames_per_s": round(frames / (t1 - t0), 1),
        "stream_bytes_per_s": round(len(data) / (t1 - t0)),
        "stream_bytes_per_frame": round(len(data) / frames) if frames else 0,
        "stream_cpu_pct": round((cpu1 - cpu0) / ((t1 - t0) * 10.0), 1) if cpu0 is not None else None,
        "stream_sync_block_ms": summary(blocks),
    }
    # 边流边打字：回合跑着时走的是 16ms 一拍的输入泵。
    echo, frame = [], []
    for index in range(8):
        e, f = type_latency(reader, master, CJK[index])
        if e is not None:
            echo.append(e)
        if f is not None:
            frame.append(f)
        time.sleep(0.1)
    burst = []
    for index in range(3):
        text = "".join(CJK[(8 + index * 6 + k) % len(CJK)] for k in range(6))
        _, f = type_latency(reader, master, text)
        if f is not None:
            burst.append(f)
        time.sleep(0.1)
    result["stream_key_echo"] = summary(echo)
    result["stream_ime6_frame"] = summary(burst)
    reader.wait_quiet(0.8, 30.0)
    clear_draft(reader, master)
    return result


def strace_frames(path):
    """strace 记下的 write(1, …)：按同步块起止切帧，数每帧几次 write、多少字节。"""
    frames, current = [], None
    pattern = re.compile(r'\d+\s+[\d.]+\s+write\(1, "(.*?)"(?:\.\.\.)?, (\d+)\) = \d+')
    for line in Path(path).read_text(errors="replace").splitlines():
        match = pattern.match(line)
        if not match:
            continue
        data, size = match.group(1), int(match.group(2))
        if "\\33[?2026h" in data[:48]:
            current = {"writes": 0, "bytes": 0}
            frames.append(current)
        if current is not None:
            current["writes"] += 1
            current["bytes"] += size
        if "\\33[?2026l" in data:
            current = None
    frames = frames[5:]
    if not frames:
        return "—"
    writes = sorted(frame["writes"] for frame in frames)
    return f"每帧 write 次数 p50 {writes[len(writes) // 2]} / max {writes[-1]}（{len(frames)} 帧）"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--turns", type=int, default=40)
    parser.add_argument("--cols", type=int, default=160)
    parser.add_argument("--rows", type=int, default=48)
    parser.add_argument("--tools", type=int, default=3)
    parser.add_argument("--skip-session", action="store_true")
    parser.add_argument("--strace", action="store_true",
                        help="套一层 strace 数每帧几次 write（这一趟的 CPU 数不作数）")
    parser.add_argument("--json", type=str, default="")
    args = parser.parse_args()
    h.COLS, h.ROWS = args.cols, args.rows
    h.ENV["MIYU_SYNC_TRACE"] = "1"
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    report = {"bin": str(h.BIN), "size": f"{args.cols}x{args.rows}", "turns": args.turns}
    trace_file = h.OUT / "smooth-strace.txt"
    prefix = ()
    if args.strace:
        prefix = ("strace", "-f", "-qq", "-e", "trace=write,writev", "-e", "signal=none",
                  "-ttt", "-o", str(trace_file))
    stub, daemon, tui, master, sink = start(prefix)
    reader = Reader(master, sink)
    try:
        time.sleep(1.5)
        report["lobby_idle"] = idle_window(reader, tui.pid, 6.0)
        report.update(key_phase(reader, master, "lobby"))
        if not args.skip_session:
            t0 = time.perf_counter()
            build_session(reader, master, args.turns, args.tools)
            report["build_session_s"] = round(time.perf_counter() - t0, 1)
            reader.wait_quiet(0.8, 20.0)
            screen = h.render(reader.snapshot())
            (h.OUT / "smooth-session.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
            report["session_idle"] = idle_window(reader, tui.pid, 4.0)
            report.update(key_phase(reader, master, "session"))
            report.update(stream_phase(reader, master, tui.pid))
            report.update(hover_phase(reader, master, tui.pid))
            report.update(wheel_phase(reader, master, tui.pid))
            first, settled = command_latency(reader, master, "/help", "/session")
            report["cmd_help_first_ms"] = round(first) if first else None
            report["cmd_help_settled_ms"] = round(settled) if settled else None
            first, settled = command_latency(reader, master, "/session", "第0轮")
            report["cmd_session_first_ms"] = round(first) if first else None
            report["cmd_session_settled_ms"] = round(settled) if settled else None
            screen = h.render(reader.snapshot())
            (h.OUT / "smooth-last.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
    finally:
        reader.stop()
        r.stop(tui, daemon, stub)
    if args.strace:
        report["strace"] = strace_frames(trace_file)
    for key, value in report.items():
        print(f"{key:32} {value}")
    if args.json:
        Path(args.json).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
