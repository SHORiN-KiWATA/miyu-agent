#!/usr/bin/env python3
"""全屏 TUI 走查：沙箱 daemon + 桩模型，真 PTY 里跑一轮对话再退出。

和 `testkit/repl-smoke` 是同一套骨架（沙箱 MIYU_HOME、桩 LLM、PTY），区别是
TUI 跑在 alt screen 上，画面要用 pyte 还原成屏幕矩阵才看得清——直接看字节流
只能看到一堆重绘。

跑法：

    cargo build
    python3 testkit/tui/run.py

产物在 ~/.cache/miyu-tui-smoke/：raw.bin（终端原始输出）、screen.txt（最后一屏）、
report.json、daemon.log。

**这些 TUI 走查只能一个一个跑**：它们共用同一个 `MIYU_HOME`（`/tmp/miyu-tui-smoke/home`）
和同一个桩模型端口，起头还会 `rmtree` 那个家目录。并行跑的话两边互相掀桌子，红成一片
而代码一点问题都没有。
"""

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

# 收缩行认法统一走 testkit/fold_summary.py(09-24 摘要改成按工具类别写)。
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fold_summary import FOLD_SUMMARY_RE, is_fold_summary  # noqa: E402,F401

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
# MIYU_BIN 指定别的二进制（A/B 对比修前修后用）。
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"

HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-tui-smoke/home"))
RUNTIME = os.environ.get("MIYU_TUI_RUNTIME", "/tmp/mx-tui")
PORT = int(os.environ.get("MIYU_TUI_PORT", "18433"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18499"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-tui-smoke"))
BASE = f"http://127.0.0.1:{PORT}"
# 32 行装不下带六行命令尾巴的展开时间线（`Worked for` 的抬头会滚出屏），加高。
COLS, ROWS = 110, 50
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME, MIYU_TUI="1")
# 被测的 TUI 跑在 pyte 模拟的 xterm 里，TERM 要说这个终端，不能照搬调用方的。从
# systemd 单元起的时候继承来的是 `TERM=linux`（cron 里干脆没有），TUI 就按 Linux 文本
# 控制台退掉超链接，也不认真彩色——`round26` 的 OSC 8 两条、`readonly_toggle` 的取色
# 一条只在那种环境里红（09-24 查实）。
if ENV.get("TERM", "") in ("", "dumb", "linux"):
    ENV["TERM"] = "xterm-256color"
ENV.setdefault("COLORTERM", "truecolor")

PROMPT = "走查一句"
# 桩模型要改的那个文件。Add File 语义，跑之前得先不存在。跟着沙箱走（默认仍是
# /tmp/miyu-tui-smoke/walk.txt）：写死一个路径的话，两个会话同时跑走查，后跑的那个
# 改文件时撞上「file already exists」，item12/22 平白变红（09-24）。
EDIT_FILE = HOME.parent / "walk.txt"
# item24 重开后按名字切回主会话用（见那一段）。
MAIN_SESSION = "走查主会话"
# 回复途中被取消的那两轮：思考正文截在哪看取消落在哪一刻，不能拿来验「点开是全文」。
CANCELLED_PROMPTS = ("取消我", "再取消一次")
# 思考正文。要够长——item06 得趁"还在想"的时候点开，看它会不会跟着刷新；
# 默认那句 35 个字，0.02s 一块地喂完不到三百毫秒，根本来不及点。
LONG_REASONING = (
    "先看一眼需求,再决定怎么下手。这段是思考正文,折叠时看不到,点开才有。"
    + "接着往下想:这一步要确认的是展开之后还会不会跟着刷新,所以这段得够长。" * 14
)
# 展开区的暗底（`expansion_paint` 用的 256 色 236 号）。
DARK_BG = "\x1b[48;5;236m"
# Nerd Font 图标落在私有区。
PUA = re.compile(r"^  ([\ue000-\uf8ff])[ \x1b]")
BAR = "┃"
# 过程时间线：收缩行 / 展开后的时间线项 / 再展开才看得到的思考正文
#（正文和 stub_llm.py 的 REASONING_TEXT 对齐）
PROC_STEP = "已思考"
THINK_BODY = "折叠时看不到"


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
    # 客户端连不上 daemon 时会**自己拉一个**，端口取自这份文件；没有它就用
    # 默认的 8300——那是本机真 daemon 的端口。撞不上端口的那个进程照样会攥着
    # 沙箱的锁，把后面每一次走查都堵死（09-21 实测：`another Miyu core is
    # already running`）。先写好，自拉的也落在沙箱端口上。
    (HOME / "state").mkdir(parents=True, exist_ok=True)
    (HOME / "state" / "daemon-launch.json").write_text(
        json.dumps({"port": PORT}), encoding="utf-8"
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


def spawn_tui():
    master, slave = pty.openpty()
    import fcntl
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    # 裸 `miyu` 直接进普通模式(09-13 起 `miyu normal` 退役,只留 `miyu dev`);
    # 沙箱配置没有 config_version,迁移会把 oobe_done 标成 true,不会撞上引导。
    process = subprocess.Popen(
        [str(BIN)], stdin=slave, stdout=slave, stderr=slave,
        env=ENV, cwd=str(HOME), preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    return process, master


SYNC_MARK = re.compile(rb"\x1b\[\?2026h|\x1b\[\?2026l")
# 每份流（sink）各记各的：扫到哪了、最后一个「块外」的位置。有的走查同时开两个 TUI。
_SYNC = {}


def frame_complete(sink):
    """这份流是不是停在一帧画完的地方：同步块外，或者最后一个块已经收尾。

    pyte 不认同步块（`ESC[?2026h` … `ESC[?2026l`）：块里画到一半就去渲染，读到的是半帧——
    模式行换了 footer 还没换、提示行画了输入框还没画（09-25 lobby_lane、goal_hint 偶发假红，
    机器一忙就撞上）。真终端按块整帧呈现，人看不到半帧。

    增量扫：从上一个「块外」的位置往后看，只扫还没收尾的那一段。流变短（换了 sink）或者
    `reset_view()`（换了 TUI 进程）时从头来。
    """
    length, boundary = _SYNC.get(id(sink), (0, 0))
    if len(sink) < length:
        boundary = 0
    depth = 0
    for match in SYNC_MARK.finditer(sink, boundary):
        if match.group(0).endswith(b"h"):
            depth += 1
        else:
            depth = max(0, depth - 1)
            if depth == 0:
                boundary = match.end()
    _SYNC[id(sink)] = (len(sink), boundary)
    return depth == 0


def finish_frame(master, sink, timeout=1.0):
    """停在一帧中间的话再读一点，读到这一帧画完为止（最多 `timeout` 秒）。"""
    import select
    deadline = time.time() + timeout
    while not frame_complete(sink) and time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.05)
        if not ready:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            return
        if not chunk:
            return
        sink.extend(chunk)


# 子代理的会话（会话项目第 3 段）：和 `subagent_visit.py` 同一套判据。
CHILD_TASK = "来自主会话的任务"
CHILD_UP = "○ 主会话"
CHILD_BADGE = "子代理 ↳1"


def in_child_session(screen):
    """切进了子代理自己的会话：第一句画成「来自主会话的任务」，footer 带徽标，任务条有「○ 主会话」。"""
    joined = "\n".join(screen)
    return CHILD_TASK in joined and CHILD_BADGE in joined and CHILD_UP in joined


def back_to_parent(master, sink, timeout=10.0):
    """`/back` 回主会话；回来了（看得见用户那句、没有徽标和「○ 主会话」）返回 True。"""
    os.write(master, b"/back")
    drain_until(master, sink, "/back", 3.0)
    os.write(master, b"\r")
    deadline = time.time() + timeout
    while time.time() < deadline:
        settle(master, sink, quiet=0.4, timeout=1.5)
        screen = render(bytes(sink))
        joined = "\n".join(screen)
        if PROMPT in joined and CHILD_BADGE not in joined and CHILD_UP not in joined:
            return True
    return False


def drain(master, seconds, sink):
    import select
    deadline = time.time() + seconds
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if not ready:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        sink.extend(chunk)
    # 定时读完可能正停在一帧中间（大厅动画一直在出帧），接着截屏读到的就是半帧。
    finish_frame(master, sink)


def click(master, sink, column, row, quiet=0.35, timeout=8.0):
    """在 (column, row) 原地点一下：按下 + 原地松开。行列都是 0 基。

    流式输出正忙的时候要把 `timeout` 调小：`settle` 等的是"连续静默"，而模型
    正在吐字时永远静不下来，默认那 8 秒会一路等到这一轮结束——想验"正在想的
    时候点开"就永远点不到。
    """
    os.write(master, f"\x1b[<0;{column + 1};{row + 1}M".encode())
    settle(master, sink, quiet=quiet, timeout=timeout)
    os.write(master, f"\x1b[<0;{column + 1};{row + 1}m".encode())
    settle(master, sink, quiet=quiet, timeout=timeout)


def wheel(master, sink, column, row, up=True, quiet=0.2, timeout=2.0):
    """在 (column, row) 滚一下滚轮。行列都是 0 基。"""
    button = 64 if up else 65
    os.write(master, f"\x1b[<{button};{column + 1};{row + 1}M".encode())
    settle(master, sink, quiet=quiet, timeout=timeout)


def settle(master, sink, quiet=0.35, timeout=8.0):
    """读到输出静默为止。

    固定时长的 `drain` 会切在一帧中间：全屏是整屏重绘，读了半帧再渲染，
    看到的是上一帧——按键的效果「凭运气」出现，断言跟着飘。等静默才是
    和「屏幕画完了」对齐的判据。
    """
    import select
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], quiet)
        if not ready:
            return True
        try:
            chunk = os.read(master, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        sink.extend(chunk)
    return False


def drain_until(master, sink, marker, timeout):
    """读到屏幕上出现 marker 为止。TUI 是整屏重绘，只能按渲染后的画面判。

    只在一帧画完时判（`frame_complete`）：返回时流停在帧边界上，调用方接着截屏读到的
    是整帧，不是半帧。
    """
    import select
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            sink.extend(chunk)
        if not frame_complete(sink):
            continue
        if marker in "\n".join(render(bytes(sink))):
            return True
    return False


def extract_osc52(raw):
    """从输出里把 OSC 52 写剪贴板的内容解出来。"""
    import base64
    import re
    match = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", raw)
    if not match:
        return None
    try:
        return base64.b64decode(match.group(1)).decode("utf-8", "replace")
    except Exception:
        return None


def drain_until_bytes(master, sink, marker, timeout, since=0):
    """读到**原始流**里出现 marker 为止。

    和 `drain_until` 的区别是不渲染。流攒到几百 KB 之后，每轮都喂一遍 pyte
    要几百毫秒——等一个转瞬即逝的状态（"正在思考"）时，光是等的开销就把它等
    没了。
    """
    import select
    needle = marker.encode()
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.05)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            sink.extend(chunk)
            if needle in bytes(sink)[since:]:
                return True
    return False


def cpu_ms(pid):
    """进程到现在用掉多少毫秒 CPU（用户态+内核态）。"""
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    except Exception:
        return None
    ticks = int(fields[11]) + int(fields[12])
    return ticks * 1000 // os.sysconf("SC_CLK_TCK")


def margin_violations(lines):
    """正文里有没有东西压进第 0–1 列的页边距。

    用户竖条 `┃` 贴第 0 列是设计（用户消息、活动区），别的都不该。压进去
    通常意味着"某段内容按整屏宽排版、被缓冲硬折了一次，续行落回第 0 列"。
    """
    bad = []
    for index, line in enumerate(lines):
        head = line[:2]
        if not head.strip() or head[0] == BAR:
            continue
        bad.append((index, line))
    return bad


_VIEW = {"screen": None, "stream": None, "fed": 0, "decoder": None}


def render(raw):
    """把原始字节流喂给 pyte，返回最后一屏的每一行。

    **增量**喂：走查后期流有一两兆，每调一次 render 都从头重放的话要几百毫秒，
    而这份代码一轮循环里要调好几次。等「正在思考」这种转瞬即逝的状态时，光是
    等的开销就足够把它等没——item06 第一版就是这么假报红的。

    流回退（换了一个 TUI 进程、另起一个 sink）就重建——但**只看长度**是不够的：
    第二个 TUI 头 4 秒吐的字节可能比第一个多，长度没回退，于是上一个进程画的
    东西还留在虚拟屏上，走查就会以为「新会话里还看得见上一句」（09-20 实测，
    白查了一轮）。换进程时请显式调 `reset_view()`。
    """
    import codecs

    if _VIEW["screen"] is None or len(raw) < _VIEW["fed"]:
        _VIEW["screen"] = pyte.Screen(COLS, ROWS)
        _VIEW["stream"] = pyte.Stream(_VIEW["screen"])
        _VIEW["decoder"] = codecs.getincrementaldecoder("utf-8")(errors="replace")
        _VIEW["fed"] = 0
    if len(raw) > _VIEW["fed"]:
        text = _VIEW["decoder"].decode(bytes(raw[_VIEW["fed"]:]))
        _VIEW["fed"] = len(raw)
        if text:
            _VIEW["stream"].feed(text)
    try:
        return [line.rstrip() for line in _VIEW["screen"].display]
    except IndexError:
        # pyte 偶尔会在宽字符被覆写成空串的格子上炸（`char[0]` 越界）。
        # 整屏重建一次通常就过去了，别让测具的库 bug 顶掉整场走查。
        reset_view()
        _VIEW["screen"] = pyte.Screen(COLS, ROWS)
        _VIEW["stream"] = pyte.Stream(_VIEW["screen"])
        _VIEW["decoder"] = codecs.getincrementaldecoder("utf-8")(errors="replace")
        _VIEW["fed"] = len(raw)
        _VIEW["stream"].feed(_VIEW["decoder"].decode(bytes(raw)))
        try:
            return [line.rstrip() for line in _VIEW["screen"].display]
        except IndexError:
            return []


def reset_view():
    """扔掉虚拟屏。换了 TUI 进程就要调一次，否则上一个进程画的东西还在。"""
    _VIEW["screen"] = None
    _SYNC.clear()
    _VIEW["stream"] = None
    _VIEW["decoder"] = None
    _VIEW["fed"] = 0


def kill_stale_daemon():
    """端口上还蹲着上一轮的 daemon 就先请它走。

    残留的那个会让这一轮的 daemon 绑不上端口（`Address already in use`），
    而客户端照样连得上——连的是**上一轮**那个，它的 MIYU_HOME 刚被这一轮
    删掉了。结果是满屏莫名其妙的红，跟代码一点关系没有（实测踩过）。
    """
    for pid in _listeners_on_port(PORT):
        try:
            os.kill(pid, 15)
        except ProcessLookupError:
            pass
        else:
            time.sleep(1.0)


def _listeners_on_port(port):
    """占着这个端口的 pid。Linux 走 `ss`，macOS 走 `lsof`。

    原来只有 `ss` 一条路，而且整个包在 `try/except: return` 里——**macOS 上没有
    `ss`，于是它静默什么都不做**，上面那段注释描述的灾难照样发生，只是没人知道
    为什么（2026-09-22 在真 Mac 上实测踩到）。静默失败比不做更糟：它看起来做了。
    """
    if sys.platform == "darwin":
        # -t 只出 pid；没有监听者时 lsof 退出码非 0、输出为空，正是我们要的。
        command = ["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"]
        pattern = r"(\d+)"
    else:
        command = ["ss", "-lntpH", f"sport = :{port}"]
        pattern = r"pid=(\d+)"
    try:
        out = subprocess.run(
            command, capture_output=True, text=True, timeout=5
        ).stdout
    except FileNotFoundError:
        # 两条路都没有就只能放过——但要说出来，别再静默。
        print(f"[run] 查不到端口 {port} 的占用者：{command[0]} 不在 PATH 上",
              file=sys.stderr)
        return []
    except Exception:
        return []
    return sorted({int(pid) for pid in re.findall(pattern, out)})


def reverse_cells(row):
    """某一行上被反显的列数。pyte 的 `reverse` 就是 SGR 7，也就是屏幕上的选区。

    选区是**画出来的**，不是剪贴板里的——只断言 OSC 52 的话，"选完松手反显就
    没了"这种回归照样全绿（用户实测撞到过）。
    """
    screen = _VIEW["screen"]
    if screen is None:
        return 0
    line = screen.buffer[row]
    return sum(1 for col in range(COLS) if line[col].reverse)


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    EDIT_FILE.parent.mkdir(parents=True, exist_ok=True)
    if EDIT_FILE.exists():
        EDIT_FILE.unlink()
    Path(RUNTIME).mkdir(exist_ok=True)
    if OUT.exists():
        # 上一轮的截图先清掉：某一步没跑到时留着旧图，看起来像"跑过了还是老
        # 样子"——这一轮就差点被自己骗过去。
        for stale in OUT.glob("*.txt"):
            stale.unlink()
    OUT.mkdir(parents=True, exist_ok=True)
    write_config()
    kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_REASONING="1",
            STUB_TOOL="1",
            STUB_ASK="1",
            STUB_SUBAGENT="1",
            STUB_EDIT="1",
            STUB_FAIL="1",
            STUB_BACKGROUND="1",
            STUB_BACKGROUND_COMMAND=(
                'for i in $(seq 1 120); do echo "后台第 $i 行"; sleep 1; done'
            ),
            STUB_EDIT_PATH=str(EDIT_FILE),
            # 子代理内层那条命令要慢：面板标题上的工具次数与词元、状态行上
            # 那串量，都只有在它还跑着的时候才看得见（用户那两条就是这么漏
            # 掉的——瞬间跑完的东西，测具根本采不到）。
            STUB_SUBAGENT_COMMAND="sleep 4; printf '子代理的命令输出\\n'",
            # 输出的字样要和命令本身不同：一样的话，"展开里有没有输出"这条
            # 断言光靠命令文本就满足了，等于什么都没验。
            STUB_SUBAGENT_BG_COMMAND="sleep 5; printf 'BGOUT走查输出\\n'",
            STUB_REASONING_TEXT=LONG_REASONING,
        ),
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
        sink = bytearray()
        drain(master, 3.0, sink)

        # 1. 进了 alt screen 才算全屏
        report["alt_screen"] = b"\x1b[?1049h" in bytes(sink)

        screen = render(bytes(sink))
        # 2. 输入区在屏幕下半部分，带竖条。
        #
        # 这条断言陈旧于**空会话大厅**（09-13 起空会话画 banner + 星空）：
        #   - 竖条不再顶在第 0 列，大厅把它整块缩进画在星空里 → `startswith` 恒假；
        #   - 输入框也不贴底，它跟在 banner 后面，底下还有一片星空。
        bar_rows = [i for i, line in enumerate(screen) if BAR in line]
        report["input_bar_rows"] = len(bar_rows)
        report["input_bar_at_bottom"] = bool(bar_rows) and max(bar_rows) >= ROWS // 2
        # 3. footer 带模型名
        report["footer_model"] = any("stub-model" in line for line in screen)

        # item01：空内容回车什么都不该发生（原来会发出一条空的、不触发回复的消息）
        #
        # **别整屏比对**：空会话大厅的星空是动画，两帧永远不相等（这条因此红了
        # 很久）。判据换成「大厅还在」——真发出去一条消息，大厅当场就退了。
        lobby = lambda rows: any("Tab" in line for line in rows)
        assert lobby(screen), f"开屏不是大厅，这条判据不成立: {screen[:40]}"
        os.write(master, b"\r")
        settle(master, sink)
        report["item01_empty_enter_sends_nothing"] = lobby(render(bytes(sink)))

        # 4. 打字 → 出现在输入框里
        os.write(master, PROMPT.encode())
        typed = drain_until(master, sink, PROMPT, 3.0)
        report["typing_echo"] = typed

        # 5. 回车 → 用户消息进历史（带竖条），桩模型回复进历史（不带竖条）
        os.write(master, b"\r")
        # 5a. 先来一个提问面板：回车选中第一项。面板是盖上去的，它退场之后
        #     「问了什么答了什么」必须留在正文里，否则等于没输出。
        report["question_panel"] = drain_until(master, sink, "走查用的问题", 30.0)
        # `drain_until` 一撞见那句话就返回,此刻面板往往只画了一半。等静默,
        # 别拿半帧当画面——这正是 `settle` 自己文档里写的那条,这儿漏了。
        # macOS 上抓到的就是半帧:停在第二个选项,「自己输入」那行和底部按键提示
        # 都还没到,于是 item14 判成「面板没贴底」(09-23 真机,Linux 上同一帧是全的)。
        settle(master, sink, quiet=0.6, timeout=15.0)
        # item10：面板是盖上去的，它在的时候正文就得还在；退场后更得在。
        during_question = render(bytes(sink))
        (OUT / "question-panel.txt").write_text(
            "\n".join(during_question), encoding="utf-8"
        )
        # item14：面板贴着屏幕底边，不该浮在半空中
        last_row = max(
            (i for i, line in enumerate(during_question) if line.strip()), default=-1
        )
        report["item14_panel_sits_at_bottom"] = last_row >= ROWS - 2
        body_during = any(
            line.startswith(BAR) and PROMPT in line for line in during_question
        )
        os.write(master, b"\r")

        # item03/06：**跑着的时候**的子代理那一行点开，切进它自己的会话（会话项目第 3 段，
        # 原来是盖在正文上的浮层），在那儿看得到它自己的过程；`/back` 回主会话。
        # 09-25 前这里还按浮层走：点开后按 Esc——新设计下 Esc 只是不看了、人还在子会话里，
        # 后面三十多条检查全落在子会话上连锁红（红绿账建账时查明）。
        report["item03_live_subagent_opens_panel"] = False
        report["item06_subagent_counts_rise"] = False
        mark_sub = len(sink)
        if drain_until_bytes(master, sink, "走查子代理", 30.0, since=mark_sub):
            sub_row = next(
                (
                    i
                    for i, line in enumerate(render(bytes(sink)))
                    if "走查子代理" in line
                ),
                None,
            )
            if sub_row is not None:
                click(master, sink, 5, sub_row, quiet=0.2, timeout=2.0)
                drain_until(master, sink, CHILD_TASK, 10.0)
                settle(master, sink, quiet=0.3, timeout=2.0)
                live_panel = render(bytes(sink))
                (OUT / "subagent-live.txt").write_text(
                    "\n".join(live_panel), encoding="utf-8"
                )
                report["item03_live_subagent_opens_panel"] = in_child_session(live_panel)
                # 子会话里看得到它自己的过程：它跑的那条命令、它想的那段，一步步长出来。
                # 换会话会先清屏再按库回放，这里看得到的都是子代理自己的。
                deadline = time.time() + 12.0
                while time.time() < deadline:
                    settle(master, sink, quiet=0.3, timeout=1.0)
                    screen_now = "\n".join(render(bytes(sink)))
                    if "运行命令" in screen_now or "已思考" in screen_now:
                        report["item06_subagent_counts_rise"] = True
                        break
                (OUT / "subagent-live-counts.txt").write_text(
                    "\n".join(render(bytes(sink))), encoding="utf-8"
                )
                back_to_parent(master, sink)

        report["reply_seen"] = drain_until(master, sink, "走查的回复", 30.0)
        drain(master, 1.5, sink)
        screen = render(bytes(sink))
        (OUT / "screen.txt").write_text("\n".join(screen), encoding="utf-8")

        # 面板退场之后问答要留在正文里
        # 照搬 inline 的样子：「已回答 N 个问题」+ 每题一行「标题：答案」
        report["question_recorded"] = any("已回答" in line for line in screen) and any(
            "走查：甲选项" in line for line in screen
        )
        report["body_visible_during_question"] = body_during
        (OUT / "after-answer.txt").write_text("\n".join(screen), encoding="utf-8")
        # item09：「询问用户」这一步要落在问答块**前面**那一段过程里。
        # 收缩行在上、问答块在下，点开收缩行能看到那一步。
        # 收缩行认 `›`，不认 `Worked for`：一段过程短到不足 0.1 秒时不报耗时。
        head_row = next((i for i, line in enumerate(screen) if "›" in line), None)
        answer_row = next((i for i, line in enumerate(screen) if "已回答" in line), None)
        report["item09_ask_step_before_answers"] = False
        if head_row is not None and answer_row is not None and head_row < answer_row:
            click(master, sink, 3, head_row)
            asked = render(bytes(sink))
            (OUT / "ask-expanded.txt").write_text("\n".join(asked), encoding="utf-8")
            report["item09_ask_step_before_answers"] = any(
                "询问用户" in line for line in asked
            )
            back = next((i for i, line in enumerate(asked) if "⌄" in line), head_row)
            click(master, sink, 3, back)
        report["item10_question_keeps_body"] = any(
            line.startswith(BAR) and PROMPT in line for line in screen
        )
        # item11：照搬 inline 的样子——「┃ 已回答 N 个问题」+ 每题一行「标题：答案」
        report["item11_answer_reads_like_repl"] = any(
            "已回答" in line and "个问题" in line for line in screen
        ) and any("走查：甲选项" in line for line in screen)
        reply_rows = [line for line in screen if "走查的回复" in line]
        report["reply_has_no_bar"] = bool(reply_rows) and all(
            not line.startswith(BAR) for line in reply_rows
        )
        echo_rows = [line for line in screen if PROMPT in line and line.startswith(BAR)]
        report["user_echo_has_bar"] = bool(echo_rows)

        # 5d. 后台任务：状态行钉在活动区里，点它要弹出日志面板（里面是它
        #     还在长的输出），Esc 关掉。
        # 一出现就抓：后台任务只跑十来秒，等静默会把它等没（状态行自己在动，
        # 屏幕根本不会静下来）。
        report["job_strip_visible"] = drain_until(master, sink, "走查后台任务", 30.0)
        jobs = render(bytes(sink))
        (OUT / "jobs.txt").write_text("\n".join(jobs), encoding="utf-8")
        strip = next(
            (i for i, line in enumerate(jobs) if "走查后台任务" in line), None
        )
        report["job_overlay"] = False
        report["job_overlay_has_output"] = False
        report["item04_overlay_has_frame"] = False
        report["item05_stop_closes_overlay"] = False
        report["item01_overlay_height_is_fixed"] = False
        report["item05_strip_stays_gone"] = False
        if strip is not None:
            mark_panel = len(sink)
            click(master, sink, 4, strip)
            # 等面板**整个画完**再采样：后台命令一直在写日志，屏幕根本静不下来，
            # `settle` 超时返回时抓到的常是半帧（少了底下那条框线，`job_overlay`
            # 这类断言就假报红）。认它自己的页脚。
            drain_until(master, sink, "Esc 关闭", 15.0)
            panel = render(bytes(sink))
            (OUT / "job-panel.txt").write_text("\n".join(panel), encoding="utf-8")
            report["job_overlay"] = any("Esc" in line for line in panel)
            report["job_overlay_has_output"] = any("后台第" in line for line in panel)
            # item04：面板要有上下两条横线圈出范围（左右不要、圆角也不要，
            # 用户拍板）。判据看**字节流**不看渲染后那一屏：后台任务一直在写
            # 日志，屏幕根本静不下来，`settle` 超时返回时抓到的往往是半帧。
            panel_bytes = bytes(sink)[mark_panel:].decode("utf-8", "replace")
            report["item04_overlay_has_frame"] = (
                "── " in panel_bytes
                and "Esc" in panel_bytes
                and "╭" not in panel_bytes.split("Esc")[0][-400:]
            )
            # item01：面板是个固定的取景窗，不跟着内容长。刚点开时里面往往只有
            # 一两行，跟着长的话就只有指甲盖那么大。
            # 面板的上下沿：抬头那条横线和带按键提示的那条横线。
            foot = next(
                (i for i, line in enumerate(panel) if "Esc" in line and "关闭" in line),
                None,
            )
            top = next(
                (
                    i
                    for i, line in enumerate(panel)
                    if "──" in line and foot is not None and i < foot
                ),
                None,
            )
            height = (
                foot - top + 1 if top is not None and foot is not None else 0
            )
            # item01：面板是个**固定**的取景窗，不跟着内容长——刚点开时里面
            # 往往只有一两行。item03：整体比原来矮三分之一（五分之三 → 五分之二）。
            report["item01_overlay_height_is_fixed"] = height >= ROWS * 2 // 5
            report["item03_overlay_is_shorter"] = 0 < height <= ROWS // 2
            # 滚轮那条挪到后台**子代理**面板那儿测：这时候正文才够长，
            # 面板上面有东西可翻。
            # item05：按 x 停掉任务之后面板该自己退出去
            os.write(master, b"x")
            settle(master, sink, quiet=0.6, timeout=10.0)
            stopped = render(bytes(sink))
            (OUT / "job-stopped.txt").write_text("\n".join(stopped), encoding="utf-8")
            # 认面板自己的页脚，不能认 `╰`——右上角那条通知也是个框。
            report["item05_stop_closes_overlay"] = not any(
                "Esc" in line and "关闭" in line for line in stopped
            )
            # item05b：状态行别闪——停掉之后盯三秒，它不该再冒出来一下。
            # （守护进程的任务快照是一秒轮询一次的，紧接着那一次还带着它。）
            deadline = time.time() + 3.0
            reappeared = False
            while time.time() < deadline:
                settle(master, sink, quiet=0.3, timeout=1.0)
                if any("走查后台任务" in line for line in render(bytes(sink))):
                    reappeared = True
                    break
            report["item05_strip_stays_gone"] = not reappeared
            os.write(master, b"\x1b")
            settle(master, sink)

        # 5b/5c. 过程收缩之后展开，逐项验：
        #   - 命令工具：时间线上一行窥视，点开是完整命令与输出（收起不能丢信息）
        #   - 子代理：点开是**覆盖层**而不是就地展开，面板里是它自己的时间线
        settle(master, sink)
        stream = bytes(sink).decode("utf-8", "replace")
        # 这条断言陈旧过两次，都记在这儿免得再错：
        #   1. 耗时 09-17 起不到一秒报**毫秒**（`reported_seconds`），只写 `s`
        #      的话在快机器上永远红；
        #   2. 命令那一步 09-17 起**抬头给 title、正文给命令**（`32227cc0`），
        #      窥视不再是 `printf …`。命令全文归 `tool_detail_has_command` 验。
        # 这儿只验「抬头上有耗时、后面还挂着一段窥视」。
        report["tool_row_has_peek"] = bool(
            re.search(r"运行命令 · [\d.]+m?s · \S", stream)
        )
        # 09-26 起收缩行按「动词了 + 次数」写（`运行了 N 次命令 · 用了 N 个工具`），英文界面是
        # `Ran N commands · N tools`；老二进制的 `Worked for … · N tools` 也认。
        report["tool_counted_in_summary"] = bool(
            re.search(
                r"Ran \d+ commands?\b|Worked for [^·]+· \d+ tools?\b|运行了 \d+ 次命令|用了 \d+ 个工具",
                stream,
            )
        )
        defaults = {
            "tool_detail_has_command": False,
            "tool_detail_has_output": False,
            "subagent_overlay": False,
            "subagent_inner_timeline": False,
            "subagent_overlay_closes": False,
        }
        report.update(defaults)

        def hit(marker, line):
            return marker(line) if callable(marker) else marker in line

        def find_row(marker, screen=None):
            screen = screen if screen is not None else render(bytes(sink))
            return next((i for i, line in enumerate(screen) if hit(marker, line)), None)

        def find_last_row(marker, screen=None):
            """最后一个匹配。

            屏上可能有好几段过程（问答那一段也有自己的 `Worked for`），而这一节
            要走查的是**带工具的那一段**，它在最下面。取第一个会点到上面那段问答
            上去，后面一串断言跟着连锁假报红（实测踩过）。
            """
            screen = screen if screen is not None else render(bytes(sink))
            return max((i for i, line in enumerate(screen) if hit(marker, line)), default=None)

        head = find_last_row(is_fold_summary)
        if head is not None:
            mark_expand = len(sink)
            click(master, sink, 3, head)
            opened = render(bytes(sink))
            (OUT / "tool-expanded.txt").write_text("\n".join(opened), encoding="utf-8")

            # item02：展开不能把正文顶没了／盖住
            report["item02_expand_keeps_body"] = any(
                "走查的回复" in line for line in opened
            )
            # `Worked for` 展开出来的是一条时间线，**不该**有底——给整条加底
            # 等于把正文一大片染色。底是留给"点开某一步"那一片的（见下面）。
            report["timeline_expansion_has_no_bg"] = DARK_BG not in bytes(sink)[
                mark_expand:
            ].decode("utf-8", "replace")
            # item09：图标是 Nerd Font 的字形（私有区），不是从通用符号里凑的
            report["item09_nerd_font_glyphs"] = any(
                PUA.match(line) for line in opened
            )
            # item03：时间线里**只有表头**可点。连线那一行（`│`）既不该提亮，
            #        也不该一点就把整条线收回去。
            # 展开会把整块内容往上顶（正文贴着活动区长），点开前算的行号在新
            # 一屏里指着别处——表头得按展开标记重新找。
            opened_head = next(
                (i for i, line in enumerate(opened) if "⌄" in line), None
            )
            rail = next(
                (
                    i
                    for i, line in enumerate(opened)
                    if opened_head is not None
                    and i > opened_head
                    and line.strip() == "│"
                ),
                None,
            )
            report["item03_rail_is_not_interactive"] = False
            if rail is not None:
                os.write(master, f"\x1b[<35;4;{rail + 1}M".encode())
                settle(master, sink)
                hovered = render(bytes(sink))
                click(master, sink, 3, rail)
                after_rail = render(bytes(sink))
                # 判据收在**连线那一行自己**身上：鼠标扫过它不该让它变样
                # （提亮），点它也不该把整条线收回去。
                #
                # 别拿整屏比：鼠标从别处挪过来时，原来那一行的提亮会撤掉，
                # 整屏当然不一样——那是对的，不是 bug。
                report["item03_rail_is_not_interactive"] = (
                    hovered[rail] == opened[rail]
                    and any("运行命令" in line for line in after_rail)
                )

            # 命令那一步
            step = find_row("运行命令", opened)
            report["item07_expansion_has_dark_bg"] = False
            if step is not None:
                mark_step = len(sink)
                click(master, sink, 5, step)
                deep = render(bytes(sink))
                # item07a：点开**一步**之后那一片要有暗底，和正文分得开
                report["item07_expansion_has_dark_bg"] = DARK_BG in bytes(sink)[
                    mark_step:
                ].decode("utf-8", "replace")
                (OUT / "tool-deep.txt").write_text("\n".join(deep), encoding="utf-8")
                report["tool_detail_has_command"] = any("printf" in l for l in deep)
                report["tool_detail_has_output"] = any("走查用的命令输出" in l for l in deep)
                # item05：展开内容不再套 `↳` / `│` 那层装饰
                report["item05_no_arrow_decorations"] = not any("↳" in l for l in deep)
                # item08/23：工具输出自己折行——没有一行压进页边距
                report["item08_tool_output_wraps"] = not margin_violations(deep)
                report["item23_no_soft_wrap_spill"] = not margin_violations(deep)
                # item07b：这一片没有嵌套块，点里面任意一行都该收起来
                inside = next(
                    (
                        i
                        for i, line in enumerate(deep)
                        if "走查用的命令输出" in line
                    ),
                    None,
                )
                report["item07_click_inside_collapses"] = False
                if inside is not None:
                    opened_count = sum(
                        1 for line in deep if "走查用的命令输出" in line
                    )
                    click(master, sink, 8, inside)
                    folded = render(bytes(sink))
                    (OUT / "tool-folded.txt").write_text(
                        "\n".join(folded), encoding="utf-8"
                    )
                    # 那一行窥视里也印着命令本身（`printf '走查用的命令输出…'`），
                    # 所以不能拿"还有没有"当判据——收起来之后条数得变少。
                    report["item07_click_inside_collapses"] = (
                        sum(1 for line in folded if "走查用的命令输出" in line)
                        < opened_count
                    )
                # 不管收没收上，都把这一步确保收回去——留着展开会让后面每一条
                # 断言都在一屏不该有的内容上跑。
                still = next(
                    (
                        i
                        for i, line in enumerate(render(bytes(sink)))
                        if "走查用的命令输出" in line and "printf" not in line
                    ),
                    None,
                )
                if still is not None:
                    click(master, sink, 8, still)

            # item12/22：改文件那一步点开是**补丁 diff**，路径只出现一次
            opened = render(bytes(sink))
            edit_row = find_row("编辑文件", opened)
            report["item12_edit_shows_diff"] = False
            report["item22_path_not_duplicated"] = False
            if edit_row is not None:
                click(master, sink, 5, edit_row)
                edited = render(bytes(sink))
                (OUT / "edit-deep.txt").write_text("\n".join(edited), encoding="utf-8")
                report["item12_edit_shows_diff"] = any(
                    "走查用的第一行" in line for line in edited
                )
                report["item22_path_not_duplicated"] = (
                    sum(1 for line in edited if "walk.txt" in line) <= 1
                )
                # 收回去：diff 留在屏上会把后面的断言全带偏
                fold = find_row("走查用的第一行")
                if fold is not None:
                    click(master, sink, 8, fold)

            # item21：跑失败的那一步要是红的
            stream_now = bytes(sink).decode("utf-8", "replace")
            report["item21_failed_step_is_red"] = bool(
                re.search(r"\x1b\[(?:1;)?3[19]m[^\n]*运行命令", stream_now)
                or re.search(r"\x1b\[38;5;(?:9|1)m[^\n]*运行命令", stream_now)
            )

            # 子代理那一步 → 切进它的会话（09-25 改，理由同上面 item03）
            sub = find_row("子代理")
            if sub is not None:
                click(master, sink, 5, sub)
                drain_until(master, sink, CHILD_TASK, 10.0)
                settle(master, sink, quiet=0.4, timeout=3.0)
                panel = render(bytes(sink))
                (OUT / "subagent-panel.txt").write_text("\n".join(panel), encoding="utf-8")
                report["subagent_overlay"] = in_child_session(panel)
                # 它说过话之后，前面那几步收成一行摘要（和主线一个规矩），摘要也算。
                report["subagent_inner_timeline"] = any(
                    ("运行命令" in line or "已思考" in line or is_fold_summary(line))
                    for line in panel
                )
                # item17：子会话里的一步也该能点开，和主线一套交互。子代理开口说正文之后，
                # 前面那几步会收成一行摘要（和主线一个规矩）；还没说话时是平铺的步。两种都认。
                # 从输入框往上找：任务条、footer 在它底下，不会误点。
                input_top = next(
                    (i for i, line in enumerate(panel) if line.startswith(BAR) and PROMPT not in line),
                    len(panel),
                )
                inner = None
                for i in range(min(input_top, len(panel)) - 1, -1, -1):
                    line = panel[i]
                    if is_fold_summary(line) or "运行命令" in line or "已思考" in line:
                        inner = i
                        break
                report["item17_subagent_step_expands"] = False
                if inner is not None:
                    before_inner = panel
                    click(master, sink, 5, inner)
                    after_inner = render(bytes(sink))
                    (OUT / "subagent-step.txt").write_text(
                        "\n".join(after_inner), encoding="utf-8"
                    )
                    report["item17_subagent_step_expands"] = after_inner != before_inner
                report["subagent_overlay_closes"] = back_to_parent(master, sink)

            back = find_last_row("⌄")
            if back is not None:
                click(master, sink, 3, back)

        # 6. 回翻：PgUp 之后要能看到更早的内容（全屏的核心价值）
        #    先灌几轮把正文撑过一屏
        # item02：AI 正在输出时，鼠标事件会不会堵在队列里。
        #
        # 量法：趁它在流式输出，猛发一串鼠标移动，紧跟一个字符，看那个字符多久
        # 才出现在输入框里。回合里的输入泵如果一个 tick 只取一个事件，这一串就
        # 得排队排上一秒——手上的感觉就是"选文字发涩"。
        #
        # 判定按倍率不按毫秒（AGENTS §5.2，09-25）：同一时刻先量一个不带鼠标事件的字符
        # 多久回显，作基线；机器忙的时候两个数一起变慢，比值不动。堵队的实现是一个 tick
        # 只取一个事件，80 个事件就是基线的几十倍。
        report["input_lag_ms"] = None
        report["input_lag_baseline_ms"] = None
        mark_lag = len(sink)
        os.write(master, "看看延迟".encode())
        os.write(master, b"\r")
        if drain_until_bytes(master, sink, "思考中", 20.0, since=mark_lag):
            os.write(master, b"Y")
            started = time.time()
            seen = drain_until_bytes(master, sink, "Y", 8.0, since=len(sink) - 1)
            report["input_lag_baseline_ms"] = int((time.time() - started) * 1000) if seen else None
            os.write(master, b"\x7f")
            burst = b"".join(
                f"\x1b[<35;{10 + (i % 40)};{10 + (i % 5)}M".encode() for i in range(80)
            )
            os.write(master, burst)
            os.write(master, b"Z")
            started = time.time()
            seen = drain_until_bytes(master, sink, "Z", 8.0, since=len(sink) - 1)
            report["input_lag_ms"] = int((time.time() - started) * 1000) if seen else None
            os.write(master, b"\x7f")
        settle(master, sink, quiet=0.6, timeout=30.0)

        # item06：回合跑着的时候 Ctrl+L 不该把视口顶空——顶完下一帧新内容接着
        # 冒出来，既没干净也没保住上文。
        report["item06_no_clear_while_streaming"] = None
        mark_clear = len(sink)
        os.write(master, "看清屏".encode())
        os.write(master, b"\r")
        if drain_until_bytes(master, sink, "思考中", 20.0, since=mark_clear):
            os.write(master, b"\x0c")
            settle(master, sink, quiet=0.4, timeout=3.0)
            during = render(bytes(sink))
            report["item06_no_clear_while_streaming"] = any(
                line.startswith(BAR) and "看清屏" in line for line in during
            )
        settle(master, sink, quiet=0.6, timeout=30.0)

        mark_stream = len(sink)
        cpu_before = cpu_ms(tui.pid)
        for index in range(8):
            os.write(master, f"第{index}句".encode())
            os.write(master, b"\r")
            # 等这一轮**自己**的回复停下来。原来等的是「走查的回复」这个标记,
            # 可它上一轮就在屏上了,于是立刻返回、下一句在上一轮还没答完时就
            # 打了进去,两句挤成一条(`第4句第5句`),后面的断言跟着全乱。
            settle(master, sink, quiet=0.6, timeout=30.0)
        settle(master, sink)
        # item13：流式输出期间一次整屏擦都不该有。
        # 「主智能体还在输出时输入框疯狂鬼畜跳动」的根因就是每个 delta 都
        # `Clear(All)` + 整屏重绘——这一段里只有自己的流式输出，没有外部输出，
        # 出现 `ESC[2J` 就是那个病复发了。
        streamed = bytes(sink)[mark_stream:]
        clears = streamed.count(b"\x1b[2J")
        report["full_clears_while_streaming"] = clears
        # 流式输出这一段一共往终端写了多少字节。整屏重排的实现会是这个数的
        # 好几倍——逐行跳过之后真正重画的只有变了的那一两行。
        report["streamed_bytes"] = len(streamed)
        # 这一段里前端烧了多少 CPU。**这才是"拖选发涩"的量尺**：字节数看不出
        # 差别（逐行 diff 早就把没变的行拦下来了），差别在每帧要不要把三十几行
        # 重新排一遍。
        cpu_after = cpu_ms(tui.pid)
        # 一串鼠标事件之后那个字符，回显不超过基线的 10 倍（基线按 40ms 起算，别让一次
        # 特别快的基线把门槛压得太低）。堵队的实现是基线的几十倍。
        lag, baseline = report["input_lag_ms"], report["input_lag_baseline_ms"]
        report["item02_input_keeps_up"] = (
            lag is not None and baseline is not None and lag <= max(baseline, 40) * 10
        )
        report["streaming_cpu_ms"] = (
            None if cpu_before is None or cpu_after is None else cpu_after - cpu_before
        )
        # 这一段里只有自己的流式输出，外加后台任务完成时的一次外部输出。
        # 每个 delta 都整屏擦的话这里会是几百——那正是「输入框疯狂鬼畜跳动」。
        report["item13_no_full_clears_while_streaming"] = clears <= 12
        whole = bytes(sink).decode("utf-8", "replace")
        # item06：跑命令那一步的图标是 `$`
        report["item06_command_glyph_is_dollar"] = bool(
            re.search(r"  \$ 运行命令", whole)
        )
        # item17：全屏下等待动画是点阵转轮，不是那条绿色横向点进度条
        report["item17_braille_spinner"] = bool(
            re.search(r"[⠁-⣿]", whole)
        ) and "\x1b[38;5;10m●" not in whole
        # item18：思考那一步的图标是原子（Nerd Font 私有区 U+F0768）
        # 思考那一步的图标是原子（MDI 段 U+F0768）
        report["item18_atom_glyph"] = chr(0xF0768) in whole
        # item18：正在回复时 Esc Esc 取消——「已取消」该是一条**通知**，
        #        不该变成正文里跟 `Worked for …` 粘在一起、还能点开的块。
        report["item18_cancel_is_a_toast"] = False
        mark_cancel = len(sink)
        os.write(master, "取消我".encode())
        os.write(master, b"\r")
        if drain_until_bytes(master, sink, "思考中", 20.0, since=mark_cancel):
            # 两下之间只隔几十毫秒：连着写一个 `\x1b\x1b` 会被解析成 Alt+Esc，
            # 而中间隔一次 `settle`（流式期间就是 8 秒）又超出了 2 秒的待发窗口
            # ——两种写法都不会触发中断。
            os.write(master, b"\x1b")
            drain(master, 0.08, sink)
            os.write(master, b"\x1b")
            settle(master, sink, quiet=0.6, timeout=20.0)
            cancelled = render(bytes(sink))
            (OUT / "turn-cancel.txt").write_text(
                "\n".join(cancelled), encoding="utf-8"
            )
            floats = any("╭" in line for line in cancelled)
            report["item18_cancel_is_a_toast"] = (
                any("已取消" in line for line in cancelled) and floats
            )
        settle(master, sink, quiet=0.6, timeout=20.0)

        # item05：Ctrl+C 打断输出之后，终端模式不能翻来翻去。
        #
        # 取消那条路原来不交接终端模式：守卫一 drop 就关 raw、**弹出键盘增强
        # 协议**（`ESC[<1u`），编辑器那边马上又推回去。这一来一回之间按下的键，
        # 终端按旧协议发、crossterm 按新协议解，解不出来就当普通字符塞进输入框
        # ——用户看到的「输入框里冒出代表按键的怪字符」。判据看字节流：取消之后
        # 那一段里不该有那个弹出序列。
        report["item05_ctrl_c_keeps_keyboard_mode"] = None
        report["item05_ctrl_c_keeps_input_clean"] = None
        mark_break_turn = len(sink)
        os.write(master, "再取消一次".encode())
        os.write(master, b"\r")
        if drain_until_bytes(master, sink, "思考中", 20.0, since=mark_break_turn):
            mark_break = len(sink)
            os.write(master, b"\x03")
            settle(master, sink, quiet=0.6, timeout=20.0)
            after_break = bytes(sink)[mark_break:].decode("utf-8", "replace")
            (OUT / "ctrl-c-turn.txt").write_text(
                after_break[-4000:], encoding="utf-8"
            )
            report["item05_ctrl_c_keeps_keyboard_mode"] = "\x1b[<1u" not in after_break
            # 输入框里也不该留下半截转义序列
            rows = render(bytes(sink))
            junk = [
                line
                for line in rows
                if line.startswith(BAR) and re.search(r"\[\d+(;\d+)*[u~ABCDmM]", line)
            ]
            report["item05_ctrl_c_keeps_input_clean"] = not junk
        settle(master, sink, quiet=0.6, timeout=20.0)

        before = render(bytes(sink))
        os.write(master, b"\x1b[5~")          # PgUp
        settle(master, sink)
        after = render(bytes(sink))
        # 回翻要真的换了内容，而不是屏幕闪了一下
        report["scrollback"] = before != after and any(line.strip() for line in after)
        # 思考行是「单行窥视刷新」：带 token 数，后面跟着思考正文的末尾一段，
        # 而且这一段每帧都在变（同一行不同内容出现多次）。
        stream = bytes(sink).decode("utf-8", "replace")
        # live 行上没有静态图标：正在跑的那一步由点阵转轮占着 logo 那一列。
        # 「展开思考内容」关着：抬头底下那扇窗（`│ <绿色正文>`）每帧在长，不再是
        # 抬头末尾那截窥视（用户 09-17：「思考行不是单行窥视，而是有滚动」）。
        windows = set(
            re.findall(r"思考中 · [^\n]{0,160}?│[^\n]{0,60}?\x1b\[38;5;10m([^\x1b]+)", stream)
        )
        report["thinking_window_refreshes"] = len(windows) >= 2
        # 点阵转轮落在**左边距**（第 0 列），logo 留在自己那一列：
        # `⠋ <logo> 抬头`——转轮后面紧跟一个空格和 logo，而不是抬头文字。
        # 看的是画出来的字节（全屏按格子重画，SGR 是它自己的），所以颜色序列
        # 只认"有若干个"，块标记可有可无，logo 认私有区字形或 `$`。
        report["spinner_in_glyph_column"] = bool(
            re.search(
                r"\x1b\[2m\x1b\[36m[⠁-⣿]\x1b\[0m (?:\x1b\[[0-9;]*m)*"
                r"(?:\x1b\]1337;miyu-block=\d+\x07)?[\ue000-\uf8ff\U000f0000-\U000fffff$] ",
                stream,
            )
        )
        (OUT / "scrolled.txt").write_text("\n".join(after), encoding="utf-8")
        os.write(master, b"\x1b[6~")          # PgDn 回底
        settle(master, sink)

        # 7. 拖选复制：注入 SGR 鼠标序列，截获 OSC 52 看剪贴板拿到了什么
        #    起点故意落在第 0 列（竖条那一格）——复制结果里不该出现 ┃
        screen = render(bytes(sink))
        target = next(
            (i for i, line in enumerate(screen) if line.startswith(BAR) and "第" in line),
            None,
        )
        (OUT / "pre-drag.txt").write_text("\n".join(screen), encoding="utf-8")
        if target is not None:
            mark = len(sink)
            os.write(master, f"\x1b[<0;1;{target + 1}M".encode())      # 按下：第 0 列
            settle(master, sink)
            os.write(master, f"\x1b[<32;40;{target + 1}M".encode())    # 拖到第 39 列
            settle(master, sink)
            os.write(master, f"\x1b[<0;40;{target + 1}m".encode())     # 松开
            settle(master, sink)
            (OUT / "drag.bin").write_bytes(bytes(sink)[mark:])
            copied = extract_osc52(bytes(sink)[mark:])
            report["drag_copied"] = bool(copied)
            report["copy_without_bar"] = bool(copied) and BAR not in copied
            (OUT / "copied.txt").write_text(copied or "", encoding="utf-8")
        else:
            report["drag_copied"] = False
            report["copy_without_bar"] = False

        # 8. 点击展开（两层）：`Worked for …` 点开是时间线，时间线里那一项
        #    再点开才是思考全文；各点一次收回去。
        #    点的是最后一个**正常跑完**的那一轮：被取消的那两轮排在它后面，它们
        #    的思考截在哪一个字看 Esc 落在哪一刻，全文验不出来（09-24 偶发红）。
        screen = render(bytes(sink))
        head = None
        prompt = ""
        for index, line in enumerate(screen):
            stripped = line.strip()
            if stripped.startswith(BAR) and stripped[1:].strip():
                prompt = stripped[1:].strip()
            if is_fold_summary(line) and prompt not in CANCELLED_PROMPTS:
                head = index
        if head is not None:
            click(master, sink, 3, head)
            opened = render(bytes(sink))
            (OUT / "expanded.txt").write_text("\n".join(opened), encoding="utf-8")
            report["proc_expands"] = any(PROC_STEP in line for line in opened)
            # 第二层：点时间线里那一项
            opened_head = next(
                (i for i, line in enumerate(opened) if "⌄" in line), head
            )
            step = next(
                (
                    i
                    for i, line in enumerate(opened)
                    if i > opened_head and PROC_STEP in line
                ),
                None,
            )
            if step is not None:
                click(master, sink, 5, step)
                deep = render(bytes(sink))
                (OUT / "expanded-deep.txt").write_text("\n".join(deep), encoding="utf-8")
                report["step_expands"] = any(THINK_BODY in line for line in deep)
                # item04：思考正文自己折行，不压页边距
                report["item04_thought_wraps_in_margin"] = not margin_violations(deep)
                # 收起里层
                step2 = next((i for i, l in enumerate(deep) if PROC_STEP in l), step)
                click(master, sink, 5, step2)
            else:
                report["step_expands"] = False
                report["item04_thought_wraps_in_margin"] = False
            # 收起外层
            closed_at = next(
                (i for i, l in enumerate(render(bytes(sink))) if "⌄" in l), head
            )
            click(master, sink, 3, closed_at)
            closed = render(bytes(sink))
            report["proc_collapses"] = not any(
                PROC_STEP in line for line in closed
            ) and any(is_fold_summary(line) for line in closed)
            # item02：收起之后正文照旧在（展开那一下不能把它吃掉）
            report["item02_collapse_restores_body"] = any(
                line.strip() for line in closed[: ROWS - 6]
            )
            # item14：收起之后正文和输入框之间不该裂一道大空档
            # 输入区是**屏幕底下**那一坨竖条；正文里的用户消息也以 `┃` 开头，
            # 拿第一条算的话量到的是"第一条用户消息上面有多空"。
            bar_top = min(
                (
                    i
                    for i, line in enumerate(closed)
                    if line.startswith(BAR) and i >= ROWS - 6
                ),
                default=ROWS,
            )
            last_body = max(
                (i for i, line in enumerate(closed[:bar_top]) if line.strip()),
                default=-1,
            )
            report["item14_no_gap_after_collapse"] = (
                last_body >= 0 and bar_top - last_body <= 2
            )
            (OUT / "collapsed.txt").write_text("\n".join(closed), encoding="utf-8")
        else:
            report["proc_expands"] = False
            report["step_expands"] = False
            report["proc_collapses"] = False
            report["item02_collapse_restores_body"] = False
            report["item14_no_gap_after_collapse"] = False
            report["item04_thought_wraps_in_margin"] = False

        # 9. /help 走的是 suspend → 往 stdout 打 → resume 这条老路，
        #    全屏下必须照样显示（这是「外部输出」的通用验证）
        os.write(master, b"/help\r")
        # 窗口给宽些：`drain_until` 一出现就返回，通过时不会多等；机器被别的
        # 编译占满时 5 秒不够，会假报红。
        report["help_output"] = drain_until(master, sink, "/compact", 20.0)
        # 斜杠命令是一次操作，不是一句话：正文里不该多出 `┃ /help` 的回显
        report["slash_not_echoed"] = not any(
            line.startswith(BAR) and "/help" in line for line in render(bytes(sink))
        )
        drain(master, 0.8, sink)
        (OUT / "screen.txt").write_text(
            "\n".join(render(bytes(sink))), encoding="utf-8"
        )

        # 10. 命令输出要进**历史**，不是一闪而过：打个字触发重画，它还得在。
        #    (全屏下直接 `println!` 的字节不在缓冲里，下一帧重画就被抹掉——
        #     这条就是钉住那个坑的。)
        os.write(master, b"a")
        settle(master, sink)
        restored = render(bytes(sink))
        (OUT / "restored.txt").write_text("\n".join(restored), encoding="utf-8")
        report["command_output_persists"] = any(
            "显示此帮助" in line for line in restored
        )
        bar_rows = [i for i, line in enumerate(restored) if line.startswith(BAR)]
        report["input_back_after_overlay"] = bool(bar_rows) and max(bar_rows) >= ROWS - 6
        os.write(master, b"\x7f")
        settle(master, sink)

        # 10a. 斜杠命令候选：打半个命令，输入框上方应当浮出一小块候选（带说明），
        #      Esc 关掉。
        os.write(master, b"/")
        settle(master, sink)
        # item15：打一个 `/` 就该出候选，不用再补个空格
        slash_only = render(bytes(sink))
        (OUT / "slash-only.txt").write_text("\n".join(slash_only), encoding="utf-8")
        report["item15_slash_alone_opens_hint"] = any("╭" in line for line in slash_only)
        os.write(master, b"mod")
        settle(master, sink)
        hinted = render(bytes(sink))
        (OUT / "cmd-hint.txt").write_text("\n".join(hinted), encoding="utf-8")
        report["command_hint_panel"] = any("/models" in line for line in hinted) and any(
            "╭" in line for line in hinted
        )
        # 鼠标动一下、AI 再吐点东西，候选面板都不该消失
        os.write(master, b"\x1b[<35;20;10M")
        settle(master, sink)
        report["command_hint_survives_mouse"] = any(
            "╭" in line for line in render(bytes(sink))
        )
        os.write(master, b"\x1b")
        settle(master, sink)
        report["command_hint_closes"] = not any(
            "╭" in line for line in render(bytes(sink))
        )
        for _ in range(4):
            os.write(master, b"\x7f")
        settle(master, sink)

        # item06：**正在想**的那一行点开之后，内容要跟着流式刷新
        #        （原来点得开，但里面的字停在点开的那一瞬间）
        report["item06_live_thought_refreshes"] = False
        mark_think = len(sink)
        os.write(master, "看思考".encode())
        os.write(master, b"\r")
        if drain_until_bytes(master, sink, "思考中", 20.0, since=mark_think):
            live_row = next(
                (
                    i
                    for i, line in enumerate(render(bytes(sink)))
                    if "思考中" in line
                ),
                None,
            )
            report["live_row_found"] = live_row is not None
            if live_row is not None:
                click(master, sink, 3, live_row, quiet=0.1, timeout=0.6)
                first = render(bytes(sink))
                settle(master, sink, quiet=0.1, timeout=1.2)
                second = render(bytes(sink))
                (OUT / "live-thought.txt").write_text(
                    "\n".join(first) + "\n---\n" + "\n".join(second),
                    encoding="utf-8",
                )
                report["item06_live_thought_refreshes"] = first != second
        settle(master, sink, quiet=0.6, timeout=30.0)

        # 10c. 输入框里的字要能选中、能复制
        os.write(master, "选我试试".encode())
        settle(master, sink)
        typed_rows = [
            i for i, line in enumerate(render(bytes(sink))) if "选我试试" in line
        ]
        if typed_rows:
            row = typed_rows[-1]
            mark = len(sink)
            os.write(master, f"\x1b[<0;3;{row + 1}M".encode())
            settle(master, sink)
            os.write(master, f"\x1b[<32;10;{row + 1}M".encode())
            settle(master, sink)
            render(bytes(sink))
            dragging = reverse_cells(row)
            os.write(master, f"\x1b[<0;10;{row + 1}m".encode())
            settle(master, sink)
            render(bytes(sink))
            released = reverse_cells(row)
            copied = extract_osc52(bytes(sink)[mark:])
            (OUT / "input-copy.txt").write_text(copied or "", encoding="utf-8")
            report["input_selection_copies"] = bool(copied) and "选我" in copied
            report["item09_selection_survives_mouseup"] = (
                dragging > 0 and released >= dragging
            )
            (OUT / "input-selection.txt").write_text(
                f"拖动中反显 {dragging} 列 / 松手后 {released} 列\n", encoding="utf-8"
            )
        else:
            report["input_selection_copies"] = False
            report["item09_selection_survives_mouseup"] = False
        report["item16_input_text_selectable"] = report["input_selection_copies"]
        for _ in range(4):
            os.write(master, b"\x7f")
        settle(master, sink)

        # 10b. 真·覆盖层：`/config` 的配置界面是「收起活动区 → 直接打 stdout →
        #     重挂」这条老路。Esc 关掉之后屏幕不能残破（用户实测报过）。
        os.write(master, b"/config\r")
        # 等它**真画出来**：配置界面要先建库、读配置，中间有一段安静，
        # `settle` 会在那段安静里返回，抓到的是上一屏（机器被占满时尤其明显）。
        drain_until(master, sink, "┌", 20.0)
        settle(master, sink, quiet=0.6, timeout=20.0)
        picker = render(bytes(sink))
        (OUT / "picker.txt").write_text("\n".join(picker), encoding="utf-8")
        report["picker_opens"] = any("设置" in line or "配置" in line for line in picker)
        os.write(master, b"\x1b")
        settle(master, sink, quiet=0.6, timeout=20.0)
        os.write(master, b"b")
        settle(master, sink)
        after_picker = render(bytes(sink))
        (OUT / "after-picker.txt").write_text("\n".join(after_picker), encoding="utf-8")
        # 正文回得来、活动区回得来，且选择器自己的行不该留在屏上
        bar_rows = [i for i, line in enumerate(after_picker) if line.startswith(BAR)]
        report["overlay_leaves_no_residue"] = bool(bar_rows) and max(bar_rows) >= ROWS - 6
        report["content_back_after_overlay"] = any(
            line.strip() for line in after_picker[: ROWS - 6]
        )
        os.write(master, b"\x7f")
        settle(master, sink)

        # item04：Esc 退出模型选择器 = 什么都没改，不该报「会话模型已更新」。
        mark_models = len(sink)
        os.write(master, b"/models\r")
        # 等它**真画出来**再按 Esc。`settle` 只是"输出静了 0.6 秒"——收起活动区
        # 之后到选择器画出来之间正好有这么一段静默，按静默走会在选择器出场前
        # 就把 Esc 发出去（第一版就这么假报红了）。
        report["models_picker_opens"] = drain_until_bytes(
            master, sink, "继承全局模型池", 20.0, since=mark_models
        )
        (OUT / "models.txt").write_text(
            "\n".join(render(bytes(sink))), encoding="utf-8"
        )
        os.write(master, b"\x1b")
        settle(master, sink, quiet=0.6, timeout=20.0)
        after_models = bytes(sink)[mark_models:].decode("utf-8", "replace")
        (OUT / "models-after.txt").write_text(after_models[-4000:], encoding="utf-8")
        report["item04_esc_claims_no_model_change"] = (
            "会话模型已更新" not in after_models and "session model updated" not in after_models
        )
        os.write(master, b"\x7f")
        settle(master, sink)

        # item08：后台子代理的状态行，时间**左边**要有一串词元数。
        report["item08_job_strip_has_tokens"] = False
        mark_bgsub = len(sink)
        os.write(master, "STUB_SUBBG 开条后台子代理".encode())
        os.write(master, b"\r")
        if drain_until_bytes(master, sink, "走查后台子代理", 40.0, since=mark_bgsub):
            # 状态行是**一直在重画**的，屏幕静不下来；而且那串量要等它真调过
            # 一次工具才有。盯着字节流等，别等静默。
            deadline = time.time() + 25.0
            hit = None
            while time.time() < deadline and hit is None:
                settle(master, sink, quiet=0.3, timeout=1.0)
                stream_bg = bytes(sink)[mark_bgsub:].decode("utf-8", "replace")
                hit = next(
                    (
                        found
                        for found in re.finditer(
                            r"走查后台子代理[^\n\x1b]*?\s(≈?[\d.]+[KM]?)  (\d+s)",
                            stream_bg,
                        )
                        # 还没调过工具时报的是 0——那不算"有量"，等真涨上去。
                        if found.group(1).lstrip("≈").rstrip("KM") not in ("0", "0.0")
                    ),
                    None,
                )
            report["item08_job_strip_has_tokens"] = hit is not None
            (OUT / "job-strip-tokens.txt").write_text(
                (hit.group(0) if hit else "没抓到")
                + "\n\n"
                + bytes(sink)[mark_bgsub:].decode("utf-8", "replace")[-4000:],
                encoding="utf-8",
            )
            # item07：后台子代理那一行点开，切进它自己的会话（会话项目第 3 段，原来是浮层），
            # 它跑完的那条命令点开要**有东西**（用户原来的抱怨：浮层里这些工具展开都没内容）。
            # 原来的 item08「面板抬头的量跟着涨」、item02「面板外滚轮翻正文」验的是浮层本身，
            # 浮层删了就撤掉（量改挂在任务条那一行上，item08_job_strip_has_tokens 验它）。
            report["item07_job_panel_step_has_content"] = False
            # 从**下往上**找：正文里那条时间线上也有一行写着它（「已后台运行
            # xxxx」），从上往下找会点到那一行去（实测就这么错了一轮）。
            # 状态行永远在最下面。
            sub_strip = max(
                (
                    i
                    for i, line in enumerate(render(bytes(sink)))
                    if "走查后台子代理" in line
                ),
                default=None,
            )
            (OUT / "bgsub-before-click.txt").write_text(
                f"sub_strip={sub_strip}\n\n" + "\n".join(render(bytes(sink))),
                encoding="utf-8",
            )
            if sub_strip is not None:
                click(master, sink, 4, sub_strip, quiet=0.2, timeout=1.5)
                drain_until(master, sink, CHILD_TASK, 10.0)
                # 要一条**跑完的**命令：还在跑的那一步点开只有命令本身。它开口说过话之后
                # 这些步会收进一行摘要里，那就先把摘要点开再找。
                step_row = None
                deadline_step = time.time() + 20.0
                panel_now = render(bytes(sink))
                while time.time() < deadline_step and step_row is None:
                    settle(master, sink, quiet=0.2, timeout=1.5)
                    panel_now = render(bytes(sink))
                    if not in_child_session(panel_now):
                        break
                    step_row = next(
                        (
                            i
                            for i in range(len(panel_now) - 1, -1, -1)
                            if "运行命令" in panel_now[i] and "运行中" not in panel_now[i]
                        ),
                        None,
                    )
                    if step_row is None:
                        fold_row = next(
                            (
                                i
                                for i in range(len(panel_now) - 1, -1, -1)
                                if is_fold_summary(panel_now[i])
                            ),
                            None,
                        )
                        if fold_row is not None:
                            click(master, sink, 6, fold_row, quiet=0.2, timeout=1.5)
                (OUT / "bgsub-panel.txt").write_text(
                    "\n".join(panel_now), encoding="utf-8"
                )
                if step_row is not None:
                    click(master, sink, 6, step_row, quiet=0.2, timeout=1.5)
                    opened_step = render(bytes(sink))
                    (OUT / "bgsub-step.txt").write_text(
                        "\n".join(opened_step), encoding="utf-8"
                    )
                    report["item07_job_panel_step_has_content"] = any(
                        "BGOUT走查输出" in line for line in opened_step
                    )
                back_to_parent(master, sink)

            # 收摊。Ctrl+C 是有梯子的：这一轮还在输出时，第一下停的是**这一轮**，
            # 任务要第二下。不停干净的话，后面那条 Ctrl+L 会因为"还在输出"被挡
            # 下来（那是设计），看着像清屏坏了。
            os.write(master, b"\x03")
            settle(master, sink, quiet=0.6, timeout=15.0)
            os.write(master, b"\x03")
            settle(master, sink, quiet=0.6, timeout=15.0)
            gone = time.time() + 10.0
            while time.time() < gone:
                settle(master, sink, quiet=0.4, timeout=1.5)
                if not any(
                    "走查后台子代理" in line for line in render(bytes(sink))
                ):
                    break

        # 11. Ctrl+L：视口顶空，但内容还在（往回翻看得到）
        os.write(master, b"\x0c")
        drain(master, 1.0, sink)
        cleared = render(bytes(sink))
        body = [line for line in cleared[: ROWS - 5] if line.strip()]
        report["clear_empties_viewport"] = not body
        os.write(master, b"\x1b[5~")
        settle(master, sink)
        report["clear_keeps_history"] = any(
            line.strip() for line in render(bytes(sink))[: ROWS - 6]
        )
        os.write(master, b"\x1b[6~")
        drain(master, 0.8, sink)

        # item05c：Ctrl+C 停掉后台任务之后，状态行不该再被画一次。
        #
        # 判据看**字节流**：闪一下就是"清掉了又写回去"，那一下在屏幕上只存在几十
        # 毫秒，按渲染后的画面采样根本抓不住；而写回去那一笔一定留在流里。
        report["item05_ctrlc_no_strip_flash"] = None
        # 先再开一条后台任务：前面那条早被 `x` 停掉了。
        os.write(master, "STUB_BG 再开一条".encode())
        os.write(master, b"\r")
        drain_until(master, sink, "走查后台任务二", 40.0)
        settle(master, sink, quiet=0.6, timeout=20.0)
        if any("走查后台任务二" in line for line in render(bytes(sink))):
            mark_ctrlc = len(sink)
            os.write(master, b"\x03")
            settle(master, sink, quiet=0.6, timeout=10.0)
            deadline = time.time() + 3.0
            while time.time() < deadline:
                settle(master, sink, quiet=0.3, timeout=1.0)
            after = bytes(sink)[mark_ctrlc:].decode("utf-8", "replace")
            # 以画出「已停止 N 个后台任务」那条提示的那一帧为界。按键写出去之后、
            # daemon 回话之前，任务还真在跑，状态行的转轮照转（负载高时这一段有好
            # 几帧），那不叫闪。提示那一帧里状态行已经清掉，从那一帧起再出现任务名
            # 才是又画了一遍。提示一直没出来，就是 Ctrl+C 没停掉任务，同样算红。
            stopped = re.search(r"已停止 \d+ 个后台任务|stopped \d+ background task", after)
            if stopped:
                frame = after.rfind("\x1b[?2026h", 0, stopped.start())
                boundary = frame if frame >= 0 else stopped.start()
                report["item05_ctrlc_no_strip_flash"] = "走查后台任务二" not in after[boundary:]
            else:
                report["item05_ctrlc_no_strip_flash"] = False
            (OUT / "ctrlc-after.txt").write_text(after[-4000:], encoding="utf-8")

        # 11b. Ctrl+C 是有优先级的：有草稿先清草稿，人留在 REPL 里。
        os.write(master, "草稿".encode())
        settle(master, sink)
        os.write(master, b"\x03")
        settle(master, sink)
        after_ctrl_c = render(bytes(sink))
        report["ctrl_c_clears_draft"] = not any("草稿" in line for line in after_ctrl_c)
        # 还活着：打个字应当照常回显
        os.write(master, "活着".encode())
        settle(master, sink)
        report["ctrl_c_keeps_repl"] = any(
            "活着" in line for line in render(bytes(sink))
        )
        # item19：Ctrl+C 走的是 inline 那套优先级（先清草稿，人留在 REPL 里），
        #         不是一下把全屏会话掀掉。
        report["item19_ctrl_c_follows_ladder"] = (
            report["ctrl_c_clears_draft"] and report["ctrl_c_keeps_repl"]
        )
        for _ in range(2):
            os.write(master, b"\x7f")
        settle(master, sink)

        # item24 重开之后要切回这一条：先起个独一份的名字，切回时按名字找。
        os.write(master, f"/rename {MAIN_SESSION}".encode())
        drain_until(master, sink, f"/rename {MAIN_SESSION}", 3.0)
        os.write(master, b"\r")
        drain_until(master, sink, "已重命名", 5.0)
        settle(master, sink)

        # 12. Ctrl+D 退出，终端要还回来
        os.write(master, b"\x04")
        drain(master, 2.0, sink)
        report["left_alt_screen"] = b"\x1b[?1049l" in bytes(sink)
        report["exited"] = tui.poll() is not None or _wait(tui, 3.0)

        (OUT / "raw.bin").write_bytes(bytes(sink))

        # item24：重开 TUI——回放要把思考带回来，也不能报 `Worked for 0.0s`
        report["item24_reopen_keeps_thoughts"] = False
        report["item24_reopen_has_no_zero_seconds"] = False
        tui, master = spawn_tui()
        again = bytearray()
        reset_view()
        # 09-20 起 `miyu` 启动开的是**新会话**（用户拍板），所以重开之后屏幕
        # 上什么都没有——要验回放就得先用 `/session` 切回刚才那条。按名字切，
        # 不走选择器：跑满一整套之后，重开的选择器偶发按第一个 j 就收掉、没切成
        # （09-24，单独复现不出来；选择器本身由 session_picker.py 验），item01/24
        # 跟着随机红。
        #
        # 等大厅**真的画出来**再敲，不能只等「安静 1 秒」：退出后马上重开，新进程
        # 要 0.7–1.5 秒才吐第一个字节，那一秒的安静里它还没进 raw 模式，敲进去的
        # 回车被行规程变成换行、按键还被原样回显（09-24 取证：`raw-reopen.bin` 开头
        # 就是测具自己敲的 `/session\r\njj`）。大厅画出来时 raw 模式早就开了。
        deadline = time.time() + 20.0
        while time.time() < deadline and not lobby(render(bytes(again))):
            settle(master, again, quiet=0.2, timeout=1.0)
        os.write(master, f"/session {MAIN_SESSION}".encode())
        drain_until(master, again, f"/session {MAIN_SESSION}", 3.0)
        os.write(master, b"\r")
        # 重开要先连 daemon 再查库回放，中间可能安静好几秒：等到正文真出现。
        deadline = time.time() + 60.0
        while time.time() < deadline:
            settle(master, again, quiet=1.0, timeout=15.0)
            if any("›" in line or "已回答" in line for line in render(bytes(again))):
                break
        replay = render(bytes(again))
        (OUT / "reopened.txt").write_text("\n".join(replay), encoding="utf-8")
        report["item24_reopen_has_no_zero_seconds"] = not any(
            is_fold_summary(line) and re.search(r"\b0\.0s\b", line) for line in replay
        )
        # item01：**工具之前**那段思考也要带回来。桩模型每轮想两次（调工具前
        # 一次、交卷前一次），而 `turns.assistant_reasoning` 那一列只留得住最后
        # 一回合那份——只按那一列回放的话，带工具的那一轮会少数一个思考
        #（用户实测：重开之后思考行消失）。
        report["item01_reopen_keeps_pre_tool_thought"] = any(
            "2 thoughts" in line or "思考了 2 次" in line for line in replay
        )
        # 回放没有计时，收缩行长这样：`› 1 tool · 1 thought`——认 `›` 不认
        # `Worked for`。
        head = next((i for i, line in enumerate(replay) if "›" in line), None)
        if head is not None:
            click(master, again, 3, head)
            reopened = render(bytes(again))
            (OUT / "reopened-expanded.txt").write_text(
                "\n".join(reopened), encoding="utf-8"
            )
            report["item24_reopen_keeps_thoughts"] = any(
                PROC_STEP in line for line in reopened
            )
            (OUT / "reopened-expanded.txt").write_text(
                "\n".join(reopened), encoding="utf-8"
            )

        # item20：提问按 Esc 取消——时间线里不该多出一条可交互的「已取消」。
        #
        # 放在重开之后跑：它要先 `/new` 开一个干净会话（桩模型的阶段表按"这一
        # 轮已经有几条工具结果"走，老会话里早就问过了不会再问）。而 `/new` 会
        # 把当前会话换掉——放在前面的话，item24 重开时回放的就是那个空会话。
        report["item20_cancel_leaves_no_tag"] = False
        os.write(master, b"/new\r")
        settle(master, again, quiet=0.6, timeout=20.0)
        os.write(master, "再问一次".encode())
        os.write(master, b"\r")
        if drain_until(master, again, "走查用的问题", 40.0):
            os.write(master, b"\x1b")
            settle(master, again)
            os.write(master, b"\x1b")
            settle(master, again, quiet=0.6, timeout=20.0)
            toast = render(bytes(again))
            (OUT / "cancel-toast.txt").write_text("\n".join(toast), encoding="utf-8")
            # 正文里不该留下任何「已取消」
            for _ in range(12):
                settle(master, again, quiet=0.6, timeout=3.0)
                os.write(master, b"x")
                settle(master, again)
                os.write(master, b"\x7f")
                settle(master, again)
                if not any("已取消" in line for line in render(bytes(again))):
                    break
            gone = render(bytes(again))
            (OUT / "cancel-gone.txt").write_text("\n".join(gone), encoding="utf-8")
            report["item20_cancel_leaves_no_tag"] = not any(
                "已取消" in line for line in gone
            )

        os.write(master, b"\x04")
        drain(master, 2.0, again)
        (OUT / "raw-reopen.bin").write_bytes(bytes(again))
    finally:
        for process in (tui, daemon, stub):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    expected = {
        "alt_screen": True,
        "input_bar_at_bottom": True,
        "footer_model": True,
        "typing_echo": True,
        "question_panel": True,
        "question_recorded": True,
        "reply_seen": True,
        "reply_has_no_bar": True,
        "user_echo_has_bar": True,
        "tool_row_has_peek": True,
        "tool_counted_in_summary": True,
        "tool_detail_has_command": True,
        "tool_detail_has_output": True,
        "subagent_overlay": True,
        "subagent_inner_timeline": True,
        "subagent_overlay_closes": True,
        "job_strip_visible": True,
        "job_overlay": True,
        "job_overlay_has_output": True,
        "scrollback": True,
        "thinking_window_refreshes": True,
        "spinner_in_glyph_column": True,
        "clear_empties_viewport": True,
        "clear_keeps_history": True,
        "ctrl_c_clears_draft": True,
        "ctrl_c_keeps_repl": True,
        "help_output": True,
        "slash_not_echoed": True,
        "command_output_persists": True,
        "command_hint_panel": True,
        "command_hint_survives_mouse": True,
        "command_hint_closes": True,
        "input_selection_copies": True,
        "item09_selection_survives_mouseup": True,
        "item03_overlay_is_shorter": True,
        "item05_ctrl_c_keeps_keyboard_mode": True,
        "item05_ctrl_c_keeps_input_clean": True,
        "item01_reopen_keeps_pre_tool_thought": True,
        "item03_live_subagent_opens_panel": True,
        "item06_subagent_counts_rise": True,
        "item08_job_strip_has_tokens": True,
        "item07_job_panel_step_has_content": True,
        "models_picker_opens": True,
        "item04_esc_claims_no_model_change": True,
        "picker_opens": True,
        "overlay_leaves_no_residue": True,
        "content_back_after_overlay": True,
        "input_back_after_overlay": True,
        "drag_copied": True,
        "proc_expands": True,
        "step_expands": True,
        "proc_collapses": True,
        "copy_without_bar": True,
        "left_alt_screen": True,
        "exited": True,
    }
    # 用户那份 24 条清单，一条一条对上号。名字里带编号，跑完一眼看得出
    # 哪一条还没好——「全绿」这种说法对二十四条缺陷是没有意义的。
    items = {
        "item09_selection_survives_mouseup": "9 松手后输入框选区还在",
        "item05_ctrl_c_keeps_keyboard_mode": "5 取消不翻键盘协议",
        "item05_ctrl_c_keeps_input_clean": "5 取消后输入框没怪字符",
        "item01_reopen_keeps_pre_tool_thought": "1 重开带回工具前那段思考",
        "item03_live_subagent_opens_panel": "3 跑着的子代理行点开进它的会话",
        "item06_subagent_counts_rise": "6 子会话里看得到它自己的过程",
        "item08_job_strip_has_tokens": "8 状态行时间左边有词元数",
        "item07_job_panel_step_has_content": "7 面板里工具那步点开有内容",
        "models_picker_opens": "4 /models 选择器开得出来",
        "item04_esc_claims_no_model_change": "4 Esc 取消不报已更新",
        "item01_empty_enter_sends_nothing": "1 空回车不发消息",
        "item02_expand_keeps_body": "2 展开不吞正文",
        "item02_collapse_restores_body": "2 收起后正文回得来",
        "item03_rail_is_not_interactive": "3 只有 Worked for 那行可交互",
        "item04_thought_wraps_in_margin": "4 思考正文折行不越边距",
        "item05_no_arrow_decorations": "5 展开内容没有 ↳ / │",
        "item06_live_thought_refreshes": "6 正在想的那行展开会刷新",
        "timeline_expansion_has_no_bg": "2 Worked for 展开不带底",
        "item07_expansion_has_dark_bg": "7 点开一步那片有暗底",
        "item07_click_inside_collapses": "7 点暗底任意处收起",
        "item08_tool_output_wraps": "8 工具输出折行",
        "item09_nerd_font_glyphs": "9 图标是 Nerd Font",
        "item10_question_keeps_body": "10 提问不吞正文",
        "item11_answer_reads_like_repl": "11 回答后的输出照搬 repl",
        "item12_edit_shows_diff": "12 改文件展开有 diff",
        "item01_overlay_height_is_fixed": "1 面板高度不跟内容",
        "item03_overlay_is_shorter": "3 面板矮了三分之一",
        "item02_input_keeps_up": "2 输出期间鼠标不堵队",
        "item05_strip_stays_gone": "5 停任务后状态行不回闪",
        "item05_ctrlc_no_strip_flash": "5 Ctrl+C 停任务不闪状态行",
        "item06_no_clear_while_streaming": "6 输出期间 Ctrl+L 不清屏",
        "item13_no_full_clears_while_streaming": "13 流式输出期间不整屏擦",
        "subagent_overlay": "13/17 点子代理那一步进它的会话",
        "item04_overlay_has_frame": "4 后台面板有圆角框",
        "item05_stop_closes_overlay": "5 停任务后面板自动退",
        "item06_command_glyph_is_dollar": "6 跑命令的图标是 $",
        "item09_ask_step_before_answers": "9 询问那步在问答块前",
        "item14_panel_sits_at_bottom": "14 提问面板贴底",
        "item14_no_gap_after_collapse": "14 收起后没有大空档",
        "item17_braille_spinner": "17 等待动画是点阵转轮",
        "item18_atom_glyph": "18 思考图标是原子",
        "item15_slash_alone_opens_hint": "15 一个 / 就出候选",
        "item16_input_text_selectable": "16 输入框里的字能选",
        "item17_subagent_step_expands": "17 子会话里的步能点开",
        "item18_cancel_is_a_toast": "18 已取消是通知",
        "item19_ctrl_c_follows_ladder": "19 Ctrl+C 有优先级",
        "item20_cancel_leaves_no_tag": "20 取消后不留可交互的 tag",
        "item21_failed_step_is_red": "21 失败的步是红的",
        "item22_path_not_duplicated": "22 路径不重复",
        "item23_no_soft_wrap_spill": "23 软换行不溢出",
        "item24_reopen_keeps_thoughts": "24 重开带回思考",
        "item24_reopen_has_no_zero_seconds": "24 重开不报 0.0s",
    }
    expected.update({key: True for key in items})
    failed = [key for key, want in expected.items() if report.get(key) != want]
    print("— 机制 —")
    for key in expected:
        if key in items:
            continue
        mark = "✓" if key not in failed else "✗"
        print(f"  {mark} {key}: {report.get(key)}")
    print("\n— 用户那 24 条 —")
    for key, label in items.items():
        mark = "✓" if key not in failed else "✗"
        print(f"  {mark} {label}  ({key}={report.get(key)})")
    print(f"\n产物：{OUT}")
    return 1 if failed else 0


def _wait(process, seconds):
    try:
        process.wait(timeout=seconds)
        return True
    except subprocess.TimeoutExpired:
        return False


if __name__ == "__main__":
    raise SystemExit(main())
