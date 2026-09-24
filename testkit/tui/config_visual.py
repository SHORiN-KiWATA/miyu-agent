#!/usr/bin/env python3
"""设置界面（`miyu config`）的版式走查：banner / 面包屑 / 两列 / 按键条 /
动画 / 窄屏降级 / 编辑态光标。

真二进制 + PTY + pyte，隔离的 MIYU_HOME，不起 daemon、不发模型请求。

注意：界面每 30ms 画一帧（星空与扫光在动），所以**不能**拿「输出停了」当判据，
只能等屏幕文本出现预期内容。

Run: python3 testkit/tui/config_visual.py --binary /absolute/path/to/miyu
"""

import argparse
import fcntl
import http.server
import json
import os
import pty
import re
import select
import struct
import sys
import shutil
import subprocess
import tempfile
import termios
import threading
import time
from pathlib import Path

import pyte
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

COLS, ROWS = 110, 36
BLOCK = "█"
STARS = "✦✶+."
# 色板里的暖金（说一声）与暖红（出错了）。真彩下 pyte 直接给 hex。
GOLD = "e4bf79"
CORAL = "e38c9a"
# 右列的值该用中灰紫（DIM），不是更暗的 FAINT——那一档是给补充说明的。
DIM = "9993a5"
FAINT = "6b6677"
# 拉模型失败时那段原话，够长才能验软换行（照着用户截图里那条写的）。
UNAUTHORIZED = json.dumps({
    "error": {"message": "Missing bearer authentication in header",
              "type": "invalid_request_error", "param": None, "code": None}
})

results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail and not ok else ''}")


class Unauthorized(http.server.BaseHTTPRequestHandler):
    """模型目录接口一律 401，用来验「拉模型失败」那行怎么显示。"""

    def do_GET(self):
        body = UNAUTHORIZED.encode()
        self.send_response(401)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Driver:
    def __init__(self, binary, home, runtime, cols=COLS, rows=ROWS):
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

        def setup():
            os.setsid()
            fcntl.ioctl(1, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), "config"],
            stdin=slave, stdout=slave, stderr=slave, cwd=home,
            # 真彩：pyte 才会把前景色原样给出来，颜色断言要用。
            env=dict(os.environ, MIYU_HOME=str(home), TERM="xterm-256color",
                     COLORTERM="truecolor", LANG="zh_CN.UTF-8",
                     XDG_RUNTIME_DIR=str(runtime)),
            preexec_fn=setup,
        )
        os.close(slave)
        self.master = master

    def pump(self, seconds=0.05):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([self.master], [], [], 0.02)[0]:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                self.stream.feed(chunk)

    def text(self):
        return "\n".join(self.screen.display)

    def fg_of(self, needle):
        """屏上含 `needle` 那一行、该段文字的前景色。

        buffer 按**列**索引，而 `find` 给的是字符下标——行里有中文时两者差着
        一截，照字符下标取会取到边上的空格（那格是 `default`）。
        """
        for y, line in enumerate(self.screen.display):
            index = line.find(needle)
            if index >= 0:
                return self.screen.buffer[y][display_width(line[:index])].fg
        return None

    def block_bounds(self):
        """正文块占了哪几行（第一行有字的到最后一行有字的，星空不算）。"""
        rows = []
        for y, line in enumerate(self.screen.display):
            stripped = re.sub(r"[✦✶+.\s]", "", line)
            if stripped:
                rows.append(y)
        return (rows[0], rows[-1]) if rows else (0, 0)

    def wait(self, *words, timeout=6.0, settle=0.8):
        """等到屏上出现这些词，**再多等一会儿**：换屏时内容是逐行落下的，
        第一眼看到的屏还差下面几行。"""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump(0.05)
            if all(word in self.text() for word in words):
                self.pump(settle)
                return self.text()
        return None

    def send(self, keys, *words, timeout=6.0, settle=0.8):
        os.write(self.master, keys)
        return self.wait(*words, timeout=timeout, settle=settle)

    def resize(self, cols, rows):
        self.screen.resize(lines=rows, columns=cols)
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.pump(0.4)

    def cpu_ticks(self):
        """这个进程到现在烧掉多少**厘秒** CPU(1 秒 100 个,所以数值就是单核百分比)。

        Linux 上 /proc/<pid>/stat 里 utime+stime 的单位正是厘秒(USER_HZ=100)。
        macOS 没有 /proc,这一句原来直接 FileNotFoundError,再把 close() 那条路
        拖垮成 TimeoutExpired——整份走查看起来像被测的东西挂了(09-23 真机)。
        `ps -o cputime=` 打的是 `分:秒.厘秒`,同一个单位,拿来减就行。"""
        try:
            with open(f"/proc/{self.process.pid}/stat") as handle:
                fields = handle.read().rsplit(") ", 1)[1].split()
            return int(fields[11]) + int(fields[12])
        except OSError:
            pass
        raw = subprocess.run(["ps", "-o", "cputime=", "-p", str(self.process.pid)],
                             capture_output=True, text=True, check=False).stdout.strip()
        if not raw:
            return 0
        days, _, clock = raw.rpartition("-")          # `1-02:03:04` 这种形态
        seconds = 0.0
        for part in clock.split(":"):
            seconds = seconds * 60 + float(part)
        if days:
            seconds += float(days) * 86400
        return int(round(seconds * 100))

    def close(self):
        """收摊。**先关 master,再等**,而且等不到也只是喊一声,不掀翻整份走查。

        macOS 上连 SIGKILL 之后 `wait(5)` 都超时:43 项检查全绿,最后炸在收摊这一步
        (09-23 真机)。收摊本来就不是被测的东西,它不该决定这份走查红还是绿——
        但也不能悄悄咽下去,所以等不到的时候把进程那一行原样打出来。"""
        try:
            os.close(self.master)
        except OSError:
            pass
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                try:
                    self.process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    state = subprocess.run(
                        ["ps", "-o", "pid=,ppid=,stat=,command=", "-p",
                         str(self.process.pid)],
                        capture_output=True, text=True, check=False,
                    ).stdout.strip()
                    print(f"! 收摊:SIGKILL 之后还等不到 {self.process.pid}"
                          f" —— ps 说 [{state or '查无此进程'}]", file=sys.stderr)


def to_main(driver):
    """回到主菜单并把光标顶回第一项。

    菜单记着上次停在哪，不复位的话「按 N 次 j」就会走到别的项上去——这一条
    让我在这份走查里连着栽了两次（第二次直接按中了「保存并退出」，程序退了，
    后面全红）。
    """
    driver.wait("供应商和模型", "保存并退出", settle=0.2)
    os.write(driver.master, b"k" * 15)
    driver.pump(0.35)


def line_with(text, *words):
    for line in text.split("\n"):
        if all(word in line for word in words):
            return line
    return ""


def display_width(text):
    import unicodedata
    return sum(2 if unicodedata.east_asian_width(ch) in "WF" else 1 for ch in text)


def value_column(line, value):
    """给定值在这一行的第几显示列。"""
    index = line.find(value)
    return display_width(line[:index]) if index >= 0 else -1


def value_start(line, label):
    """「名字 + 两个以上空格 + 值」里，值从第几显示列起笔。

    必须从名字往右找：左边距铺着星空，`\s{2,}` 从行首扫会扫到星星之间的空白。
    """
    index = line.find(label)
    if index < 0:
        return -1
    head = index + len(label)
    match = re.match(r"\s{2,}\S", line[head:])
    return display_width(line[: head + match.end() - 1]) if match else -1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    sandbox = sandbox_dir.make("miyu-config-visual-")
    out = Path(os.environ.get("OUT") or Path.home() / ".cache" / "miyu-config-visual")
    out.mkdir(parents=True, exist_ok=True)
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    # 运行时目录**不能**挂在沙箱底下。daemon 的 IPC socket 落在
    # `<runtime>/miyu-<12 位摘要>/core.sock`,而 unix socket 的路径在 macOS 上
    # 顶格 104 字节;那儿的默认 TMPDIR 是
    # `/var/folders/dz/6m5cg9zj6fb081f1r4f75gmr0000gn/T/`,光前缀就 48 字节,
    # 算下来 113,daemon 直接起不来(09-23 在真机上量的)。所以给它单独找个短的。
    runtime = Path(tempfile.mkdtemp(prefix="mx-cv-", dir="/tmp"))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Unauthorized)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    denied = f"http://127.0.0.1:{server.server_address[1]}/v1"
    (home / "config/config.jsonc").write_text(json.dumps({
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [
            {"id": "stub", "display_name": "Stub",
             "base_url": "http://127.0.0.1:1/v1", "protocol": "openai-chat",
             "api_key": "stub", "models": ["stub-model"]},
            {"id": "denied", "display_name": "Denied", "base_url": denied,
             "protocol": "openai-chat", "api_key": "nope", "models": []},
        ],
        "display": {"language": "zh"},
        "memory": {"enabled": False},
    }))

    driver = Driver(args.binary.resolve(), home, runtime)
    try:
        text = driver.wait("供应商和模型", "保存并退出")
        check(text is not None, "主菜单画出来了")
        text = text or driver.text()

        # ── 屏顶 ──
        banner_rows = sum(1 for line in text.split("\n") if BLOCK in line)
        check(banner_rows >= 5, "banner 在屏顶", f"只有 {banner_rows} 行含 {BLOCK}")
        check("A G E N T" in text, "banner 副标题在")
        check("◉ 配置" in line_with(text, "◉ 配置"), "面包屑显示当前层")

        # ── 正文两列 ──
        row = line_with(text, "配置全局文本模型", "当前")
        check(bool(row), "菜单行带上了当前值")
        gap = re.search(r"配置全局文本模型(\s+)当前", row)
        check(gap is not None and len(gap.group(1)) >= 2, "名字与值分成两列",
              f"间隔 {len(gap.group(1)) if gap else 0} 格")
        column_a = value_column(row, "当前: Stub")
        column_b = value_column(line_with(text, "接入通讯平台"), "未启用")
        check(column_a > 0 and column_a == column_b, "各行右列对齐",
              f"{column_a} vs {column_b}")

        # ── 屏底 ──
        check("─────" in text, "正文与按键条之间有细线")
        keybar = line_with(text, "移动")
        check("⏎" in keybar and "Esc" in keybar, "按键条列着按键", keybar.strip()[:60])
        check("1/10" in text, "右下角有计数")

        # ── 星空 ──
        left_margin = [line[:10] for line in text.split("\n")]
        check(any(ch in "".join(left_margin) for ch in STARS), "两侧铺了星空")

        # ── 整块垂直居中 ──
        top, bottom = driver.block_bounds()
        check(abs(top - (ROWS - 1 - bottom)) <= 2, "整块上下居中",
              f"上边 {top} 行、下边 {ROWS - 1 - bottom} 行")

        # ── 动画：两帧的星空不一样 ──
        first = driver.text()
        driver.pump(0.8)
        second = driver.text()
        check(first != second, "星空/扫光每帧在动")

        # ── 连着按键也不该把动画顶停（帧号按时间算，不是数帧）──
        before_keys = driver.text()
        for _ in range(12):
            os.write(driver.master, b"j" if _ % 2 else b"k")
            driver.pump(0.06)
        check(before_keys != driver.text(), "连着按键，星空照样在动")
        # 连按把光标挪走了，下面的导航按次数走，先回到第一项。
        os.write(driver.master, b"k" * 15)
        driver.pump(0.4)

        # ── CPU：动画不能把一个核吃满 ──
        before = driver.cpu_ticks()
        driver.pump(1.0)
        ticks = driver.cpu_ticks() - before
        check(ticks <= 40, f"动画的占用还好（1 秒 {ticks} 个 tick ≈ {ticks}% 单核）")

        # ── 进一层：面包屑跟着走 ──
        to_main(driver)
        text = driver.send(b"j" * 7 + b"\r", "工具启用", "界面语言")
        check(text is not None, "进得了全局设置")
        text = text or driver.text()
        check("● 配置 ── ◉ 全局设置" in line_with(text, "◉ 全局设置"),
              "面包屑记着从哪儿进来的")
        values = [value_start(line_with(text, label), label)
                  for label in ("工具最大轮数", "界面语言", "命令显示行数")]
        check(len(set(values)) == 1 and values[0] > 0, "表单右列对齐", str(values))
        check("导航中" in text, "横线上方写着现在什么状态")
        check(driver.fg_of("简体中文") == DIM, "右列的值不压到最暗那档",
              f"值是 {driver.fg_of('简体中文')}，FAINT 是 {FAINT}")
        rows = text.split("\n")
        status_row = next(index for index, line in enumerate(rows) if "导航中" in line)
        above = re.sub(r"[✦✶+.\s]", "", rows[status_row - 1])
        check(above == "", "正文与底下那行之间空一行", f"上一行是「{rows[status_row - 1].strip()[:40]}」")

        # ── 换层不再逐行铺开：一眼就是整屏 ──
        os.write(driver.master, b"\x1b")
        to_main(driver)
        started = time.monotonic()
        os.write(driver.master, b"j" * 7 + b"\r")
        # 最后一项出现得多快。逐行落下的话，18 行要 18 帧（≈540ms）才铺完。
        while time.monotonic() - started < 3.0:
            driver.pump(0.02)
            if "终端集成会话默认模式" in driver.text():
                break
        spent = time.monotonic() - started
        check(spent < 0.35, f"换层一次画完（{spent * 1000:.0f}ms）",
              f"{spent * 1000:.0f}ms，像是还在逐行铺")

        # ── 编辑态：光标落在值上 ──
        text = driver.send(b"jj\r", "编辑中")  # 09-23:语言排第一,数字字段退到第三行
        check(text is not None, "进得了编辑态")
        caret_x = driver.screen.cursor.x
        check(caret_x >= values[0], "光标落在值那一列", f"光标 {caret_x} < 值列 {values[0]}")
        driver.send(b"\x1b", "导航中")

        # ── 退一层：面包屑弹回去 ──
        text = driver.send(b"\x1b", "供应商和模型", "保存并退出")
        check(text is not None and "◉ 配置" in text and "全局设置" not in text,
              "退回上一层，面包屑跟着弹")

        # ── 功能表：跟引导的功能表同一个样子（2026-09-20 起插件表并进了这里）──
        to_main(driver)
        text = driver.send(b"j" * 5 + b"\r", "启用的功能", "当前人格")
        check(text is not None, "进得了人格菜单")
        text = driver.send(b"j\r", "内置插件", "子系统")
        check(text is not None, "进得了功能表")
        text = text or driver.text()
        check("[*]" in text, "功能开关是 [*]")
        check(
            "● 配置 ── ● 人格和功能 ── ◉ 启用的功能" in line_with(text, "◉ 启用的功能"),
            "功能表的面包屑记着两层",
        )
        driver.send(b"\x1b", "当前人格")
        driver.send(b"\x1b", "保存并退出")

        # ── 三列：表头 + 按键条折行 ──
        to_main(driver)
        text = driver.send(b"\r", "供应商", "组织", "模型")
        check(text is not None, "进得了供应商/模型三列")
        text = text or driver.text()
        header = line_with(text, "供应商", "组织", "模型")
        check(bool(header), "三列表头在同一行", header.strip()[:60])
        rows_with_keys = [line for line in text.split("\n")
                          if "切栏" in line or "刷新" in line]
        check(len(rows_with_keys) >= 2, "按键条放不下就折行",
              f"只有 {len(rows_with_keys)} 行")
        driver.send(b"q", "保存并退出")

        # ── 拉模型失败：折行 + 暖红 ──
        to_main(driver)
        text = driver.send(b"\r", "供应商", "组织", "模型")
        check(text is not None, "再进一次三列")
        # Denied 那家在列表里第几个不写死：一格一格往下走，走到 401 那家为止
        # （路上会先经过连不上的 Stub，它那句报错短，一行就够，验不了折行）。
        text = None
        for _ in range(12):
            os.write(driver.master, b"j")
            text = driver.wait("401", timeout=2.5, settle=0.3)
            if text:
                break
        text = text or driver.text()
        (out / "error.txt").write_text(text)
        check("获取模型失败" in text, "拉不到模型会说一声")
        # 折了行 = 开头那半和结尾那半不在同一行上。
        head_line = line_with(text, "获取模型失败")
        tail_index = next((index for index, line in enumerate(text.split("\n"))
                           if "code" in line and "获取模型失败" not in line), -1)
        error_lines = [line for line in text.split("\n")
                       if "获取模型失败" in line or ("code" in line and "401" not in line)]
        check(tail_index >= 0 and "code" not in head_line, "长报错软换行",
              f"整条挤在一行里：{head_line.strip()[:70]}")
        def last_text_col(line):
            return display_width(re.sub(r"[✦✶+.\s]+$", "", line))

        widest = max((last_text_col(line) for line in error_lines), default=999)
        check(widest <= COLS - 8, "报错没冲出正文区", f"最宽到第 {widest} 列")
        check(driver.fg_of("获取模型失败") == CORAL, "报错是暖红",
              str(driver.fg_of("获取模型失败")))
        rows = text.split("\n")
        error_row = next(index for index, line in enumerate(rows) if "获取模型失败" in line)
        above = re.sub(r"[✦✶+.\s]", "", rows[error_row - 1])
        check(above == "", "报错与列表之间也空一行",
              f"上一行是「{rows[error_row - 1].strip()[:40]}」")
        driver.send(b"q", "保存并退出")

        # ── 矮终端：banner 塌成一行，正文还在 ──
        driver.resize(100, 22)
        text = driver.wait("供应商和模型")
        check(text is not None, "矮终端下正文还在")
        text = text or driver.text()
        check(BLOCK not in text, "矮终端把 banner 收掉了")
        check("M I Y U" in text, "收掉 banner 也留一行标题")
        check("保存并退出" in text or "语音功能" in text, "矮终端没把菜单挤没")

        # ── 回到正常尺寸，画面要自己长回来 ──
        driver.resize(COLS, ROWS)
        text = driver.wait("供应商和模型")
        check(text is not None and BLOCK in text, "拉回大窗口 banner 回来了")
    finally:
        (out / "last.txt").write_text(driver.text())
        driver.close()
        shutil.rmtree(runtime, ignore_errors=True)

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({out})")
    raise SystemExit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
