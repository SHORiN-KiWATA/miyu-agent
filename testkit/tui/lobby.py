#!/usr/bin/env python3
"""空会话大厅的输入框走查：窄框里的换行、光标落点、缩行时的 footer 残影。

和 `testkit/tui/run.py` 同一套骨架（沙箱 MIYU_HOME、桩模型、真 PTY、pyte），
只盯一件事：**大厅里输入框只有终端的三分之二宽**，测行数、画字、算光标、
擦旧行必须用同一个宽度。

两条断言是分开的：

1. 光标：打一段超出窄框、但没超出终端的话，文字会折行。光标必须留在窄框
   里、落在最后一行上。修复前它按整终端宽算，停在框右边的星空上。
2. footer 唯一：一次删一个字让输入区从两行缩回一行。**每读到一块输出就查
   一次**——残影是转瞬即逝的（下一帧星星一动就被盖掉），只看最后静止的那
   一屏什么都验不到。

跑法：

    cargo build
    python3 testkit/tui/lobby.py

产物在 ~/.cache/miyu-lobby-smoke/：screen.txt（最后一屏）、report.json、
ghost.txt（抓到残影时那一屏）、daemon.log。
"""

import codecs
import json
import os
import pty
import re
import shutil
import struct
import subprocess
import sys
import termios
import time
import urllib.error
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

try:
    import pyte
except ImportError:
    print("! 需要 pyte：pip install --user pyte", file=sys.stderr)
    raise SystemExit(2)

ROOT = Path(__file__).resolve().parents[2]
# 验「修复前会红」时把 MIYU_BIN 指到旧构建上，别的都不用改。
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"

HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-lobby-smoke/home"))
RUNTIME = os.environ.get("MIYU_LOBBY_RUNTIME", "/tmp/mx-lobby")
PORT = int(os.environ.get("MIYU_LOBBY_PORT", "18435"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18497"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-lobby-smoke"))
BASE = f"http://127.0.0.1:{PORT}"
COLS, ROWS = 110, 44
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME, MIYU_TUI="1")

BAR = "┃"
# 同步更新的收尾。TUI 的每一帧都裹在 `\x1b[?2026h` … `\x1b[?2026l` 里，
# 读到它才算「这一帧画完了」。
FRAME_END = b"\x1b[?2026l"
# footer 上的模型名。屏幕上出现几次 = 屏幕上挂着几条 footer。
FOOTER_NEEDLE = "stub-model"
# 这一段的显示宽度是 84 列：窄框（110 的三分之二 ≈ 73，去掉提示前缀剩 71）
# 放不下，整个终端（去掉前缀 108）放得下——两种宽度算出来的行数必然不同，
# 用错哪一个都藏不住。
SENTENCE = "你试试基于Miyu的landlock功能在Projects目录里新建一个简易的小工具然后再把它跑起来看看"
HEAD = SENTENCE[:6]
TAIL = SENTENCE[-3:]


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub",
            "display_name": "Stub",
            "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat",
            "api_key": "stub",
            "models": ["stub-model"],
        }],
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def wait_http(url, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.2)
    return False


def kill_stale_daemon():
    """端口上还蹲着上一轮的 daemon 就先请它走（理由见 run.py 同名函数）。"""
    try:
        out = subprocess.run(
            ["ss", "-lntpH", f"sport = :{PORT}"],
            capture_output=True, text=True, timeout=5,
        ).stdout
    except Exception:
        return
    for pid in set(re.findall(r"pid=(\d+)", out)):
        try:
            os.kill(int(pid), 15)
        except ProcessLookupError:
            pass
    if out.strip():
        time.sleep(1.0)


def spawn_tui():
    master, slave = pty.openpty()
    import fcntl
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(BIN)], stdin=slave, stdout=slave, stderr=slave,
        env=ENV, cwd=str(HOME), preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    return process, master


class View:
    """一块终端画面。字节按**绘制交易**喂进去。

    TUI 的每一次重绘都裹在同步更新里（`\x1b[?2026h` … `\x1b[?2026l`），
    交易结束才是「这一帧画完了」。按读到的 chunk 边界看屏幕是错的：读会切在
    一帧中间，看到的是「banner 画完了、输入框还没画」的半成品——第一版测具
    就是这么把光标读成艺术字里的某一格、把输入框读没了的。

    所以这里攒着字节，凑够一整笔交易才喂给 pyte，`on_frame` 每帧调一次。
    大厅的星星 40ms 动一次、输出永远静不下来，「等静默」在这儿是不成立的。
    """

    def __init__(self):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.pending = bytearray()
        self.frames = 0
        self.on_frame = None

    def feed(self, chunk):
        self.pending.extend(chunk)
        while True:
            at = self.pending.find(FRAME_END)
            if at < 0:
                return
            cut = at + len(FRAME_END)
            self._feed_bytes(bytes(self.pending[:cut]))
            del self.pending[:cut]
            self.frames += 1
            if self.on_frame is not None:
                self.on_frame(self)

    def _feed_bytes(self, chunk):
        text = self.decoder.decode(chunk)
        if text:
            self.stream.feed(text)

    def resize(self, cols, rows):
        """跟着终端一起改尺寸。pyte 的屏幕不跟着改的话，之后读到的行是按旧宽
        度切的，断言全成了看旧画面。"""
        self.screen.resize(rows, cols)

    def lines(self):
        """当前这一屏，每行一条字符串。

        不用 pyte 的 `display`：帧读到一半时缓冲里可能留着空 data 的格子
        （宽字符的后半格），`display` 会在那上面 `IndexError`。逐格取 data
        自己拼，空格子自然不占字符——而这份测具正是要在帧中间看屏幕的。
        """
        buffer = self.screen.buffer
        rows = []
        for y in range(self.screen.lines):
            line = buffer[y]
            rows.append(
                "".join(line[x].data for x in range(self.screen.columns)).rstrip()
            )
        return rows

    def cursor(self):
        return self.screen.cursor.x, self.screen.cursor.y

    def footers(self):
        return [i for i, line in enumerate(self.lines()) if FOOTER_NEEDLE in line]


def pump(master, view, seconds, watch=None, until=None):
    """读 `seconds` 秒，按帧推进画面。

    `watch` 每画完一帧调一次（残影只在某一帧上出现，逐帧查才抓得住）；
    `until` 返回真就提前收工。返回 `until` 有没有满足过。
    """
    import select
    previous = view.on_frame
    hit = [False]

    def on_frame(v):
        if watch is not None:
            watch(v)
        if until is not None and until(v):
            hit[0] = True

    view.on_frame = on_frame
    try:
        deadline = time.time() + seconds
        while time.time() < deadline:
            if hit[0]:
                break
            ready, _, _ = select.select([master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            view.feed(chunk)
    finally:
        view.on_frame = previous
    return hit[0]


def input_box(view):
    """输入框在屏幕上的 (左列, 右列)：按带竖条那几行的竖条位置认。

    右边界按「框是居中的」反推：`left + width = cols - left`。框两侧就是星空，
    直接量行长会把星星算进来（第一版就是这么把右边界量到 102 的）。
    """
    from collections import Counter

    # 认「哪一列上有竖条」而不是「行首是不是竖条」：框左边就是星空，
    # 随机飘一颗星到竖条左边，按行首认就整个认不出来了（实测踩过）。
    columns = Counter(line.index(BAR) for line in view.lines() if BAR in line)
    if not columns:
        return None
    left = columns.most_common(1)[0][0]
    return left, view.screen.columns - left


def resize_pty(master, cols, rows):
    """改 PTY 的窗口大小。内核会给前台进程组送 SIGWINCH，TUI 自己重排。"""
    import fcntl
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def input_text_length(view, box):
    """窄框里现在有几个字。

    **只数框里那一段**：框右边就是星空，按「竖条之后的整行」数会把星星当成
    没删干净的输入（实测踩过）。
    """
    left, right = box
    buffer = view.screen.buffer
    total = 0
    for y in range(view.screen.lines):
        row = buffer[y]
        if row[left].data != BAR:
            continue
        text = "".join(row[x].data for x in range(left + 2, right)).strip()
        if not text or FOOTER_NEEDLE in text:
            continue
        total += len(text)
    return total


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(parents=True, exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    for stale in OUT.glob("*.txt"):
        stale.unlink()
    write_config()
    kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(STUB_PORT)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = None
    tui = None
    report = {}
    try:
        if not wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=ENV, cwd=str(HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not wait_http(f"{BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        tui, master = spawn_tui()
        view = View()
        # 等大厅真的画出来：输入框的竖条出现才算就位。
        ready = pump(master, view, 15.0, until=lambda v: v.footers())
        report["lobby_ready"] = ready

        report["alt_screen"] = True  # 进不了全屏下面全都无从谈起，由 lobby_box 兜底
        box = input_box(view)
        report["lobby_box"] = box
        # 大厅：输入框是居中窄框，不贴第 0 列，也没铺到终端右边。
        report["lobby_is_narrow"] = bool(box) and box[0] > 0 and box[1] < COLS - 1

        # ---- 1. 窄框里的换行与光标 ----
        os.write(master, SENTENCE.encode())
        # 等到句尾那几个字真的落到屏幕上的那一帧。
        pump(master, view, 10.0, until=lambda v: any(TAIL in line for line in v.lines()))
        # 再多收几帧：光标是在一帧的最后才归位的。
        pump(master, view, 1.0)
        (OUT / "typed.txt").write_text("\n".join(view.lines()), encoding="utf-8")
        cursor_x, cursor_y = view.cursor()
        box = input_box(view) or (0, COLS)
        lines = view.lines()
        head_row = next((i for i, line in enumerate(lines) if HEAD in line), None)
        tail_row = next((i for i, line in enumerate(lines) if TAIL in line), None)
        report["cursor"] = [cursor_x, cursor_y]
        report["head_row"] = head_row
        report["tail_row"] = tail_row
        # 文字确实折了行：窄框放不下这一句，句尾落到了下一行。
        report["wrapped"] = (
            head_row is not None and tail_row is not None and tail_row > head_row
        )
        # 光标留在窄框里，且落在最后一行文字上。修复前它按整终端宽算，
        # 停在框右边的星空里、还在第一行。
        report["cursor_inside_box"] = box[0] <= cursor_x <= box[1]
        report["cursor_on_last_text_row"] = tail_row is not None and cursor_y == tail_row

        # ---- 2. 缩行时 footer 只有一条 ----
        ghosts = []

        def watch(v):
            rows = v.footers()
            if len(rows) > 1:
                ghosts.append((list(rows), list(v.lines())))

        # 先确认起点只有一条：起点就脏的话下面验的是别的东西。
        report["one_footer_before"] = len(view.footers()) == 1
        # 一次一个 Backspace 删回去，跨过「两行 → 一行」那个坎。
        for index in range(len(SENTENCE)):
            os.write(master, b"\x7f")
            want = len(SENTENCE) - index - 1
            pump(
                master, view, 2.0, watch=watch,
                until=lambda v, n=want: input_text_length(v, box) <= n,
            )
        pump(master, view, 1.5, watch=watch)
        report["input_emptied"] = input_text_length(view, box) == 0
        report["one_footer_after"] = len(view.footers()) == 1
        report["ghost_frames"] = len(ghosts)
        report["no_duplicate_footer"] = not ghosts
        if ghosts:
            rows, lines = ghosts[0]
            (OUT / "ghost.txt").write_text(
                f"# footer 出现在第 {rows} 行\n" + "\n".join(lines), encoding="utf-8"
            )

        # ---- 3. 拖动终端大小 ----
        # 窄框宽度跟着终端走，整块要重新居中；重排的那几帧里同样只能有一条 footer。
        for cols, rows in ((84, 40), (COLS, ROWS)):
            resize_pty(master, cols, rows)
            view.resize(cols, rows)
            pump(master, view, 3.0, watch=watch,
                 until=lambda v: len(v.footers()) == 1 and input_box(v) is not None)
        resized = input_box(view)
        report["box_after_resize"] = resized
        report["resize_keeps_one_footer"] = len(view.footers()) == 1
        # 回到原尺寸就该回到原来的位置。
        report["resize_restores_box"] = resized == box

        (OUT / "screen.txt").write_text("\n".join(view.lines()), encoding="utf-8")
        os.write(master, b"\x03")
        pump(master, view, 1.0)
        os.write(master, b"\x04")
    finally:
        for process in (tui, daemon, stub):
            if process is None:
                continue
            try:
                process.terminate()
                process.wait(timeout=5)
            except Exception:
                try:
                    process.kill()
                except Exception:
                    pass

    checks = [
        "lobby_ready",
        "lobby_is_narrow",
        "wrapped",
        "cursor_inside_box",
        "cursor_on_last_text_row",
        "one_footer_before",
        "input_emptied",
        "one_footer_after",
        "no_duplicate_footer",
        "resize_keeps_one_footer",
        "resize_restores_box",
    ]
    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    failed = [name for name in checks if not report.get(name)]
    for name in checks:
        print(f"{'✓' if report.get(name) else '✗'} {name}")
    print(f"\n产物：{OUT}")
    if failed:
        print(f"\n! 没过：{', '.join(failed)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
