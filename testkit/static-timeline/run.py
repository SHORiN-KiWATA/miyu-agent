#!/usr/bin/env python3
"""静态时间线走查：单次 `miyu "…"`（= shellhook 那条路）在真 PTY 里跑一轮。

沙箱 daemon + 桩模型，不花额度。看的是普通终端里（不是全屏）过程怎么排：

- 每一步跑完当场落进 scrollback，步与步之间一根 `│`；
- 命令那一步 `$ 运行命令 · 秒数 · 命令`，底下就地印输出尾巴；
- 编辑那一步底下是 diff；报错那一步打叉标红、印几行错误输出；
- 跑着的时候 live 区是「`│` + 转轮行（+ 命令输出尾巴）」；
- 没有 `Worked for …`，也没有 inline 那套 `工具×1 ok / ↳` 卡片。

    cargo build
    python3 testkit/static-timeline/run.py

第二轮验 Ctrl+C：命令跑到一半打断，它得收成一步「已中断」，而不是漏出
inline 的命令卡片。

产物在 ~/.cache/miyu-static-timeline/：raw-*.bin（原始字节）、screen-*.txt
（pyte 还原的最后一屏）、live-*.txt（跑着时抓的几帧）、report.json。
"""

import json
import os
import pty
import re
import select
import shutil
import signal
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
from fold_summary import is_fold_summary  # noqa: E402

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
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-static-timeline/home"))
RUNTIME = os.environ.get("MIYU_ST_RUNTIME", "/tmp/mx-st")
PORT = int(os.environ.get("MIYU_ST_PORT", "18443"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18497"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-static-timeline"))
BASE = f"http://127.0.0.1:{PORT}"
COLS, ROWS = 110, 200
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME)
EDIT_FILE = Path("/tmp/miyu-static-timeline/walk.txt")
PROMPT = "走查一句"
BRAILLE = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
# 时间线那一行的形状：`<图标> <名字> · …`。图标是 Nerd Font 私有区字形或 `$`/`⌄`。
STEP = re.compile(r"^  (?:[\ue000-\uf8ff\U000f0000-\U000fffff]|\$|✗) \S")


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
    # ST_EXPAND_REASONING=1：「展开思考内容」开着——正在想时正文得跟在抬头底下流。
    if os.environ.get("ST_EXPAND_REASONING"):
        config["display"] = {"expand_reasoning": True}
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


def spawn_one_shot(message):
    master, slave = pty.openpty()
    import fcntl
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(BIN), message], stdin=slave, stdout=slave, stderr=slave,
        env=ENV, cwd=str(HOME), preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    return process, master


def render(raw):
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)
    # 键盘增强协议的推入/弹出（`CSI > 1 u` / `CSI < 1 u`）pyte 不认，会把 `1u`
    # 当正文画出来；真终端里它们不可见。
    text = re.sub(r"\x1b\[[<>]\d*u", "", raw.decode("utf-8", "replace"))
    stream.feed(text)
    lines = [line.rstrip() for line in screen.display]
    while lines and not lines[-1]:
        lines.pop()
    return lines


def strip_ansi(text):
    text = re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", "", text)
    text = re.sub(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", "", text)
    return text


def run_turn(process, master, on_snapshot=None, interrupt_when=None, timeout=60):
    """读到进程退出为止。每隔一会儿把当前画面抓一帧交给 `on_snapshot`。"""
    sink = bytearray()
    snapshots = []
    deadline = time.time() + timeout
    last_snapshot = 0.0
    interrupted = False
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
        now = time.time()
        if now - last_snapshot > 0.25:
            last_snapshot = now
            frame = render(bytes(sink))
            snapshots.append(frame)
            if interrupt_when and not interrupted and interrupt_when(frame):
                os.write(master, b"\x03")
                interrupted = True
        if process.poll() is not None:
            # 进程退了，把剩下的读干净。
            for _ in range(10):
                ready, _, _ = select.select([master], [], [], 0.1)
                if not ready:
                    break
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                sink.extend(chunk)
            break
    return bytes(sink), snapshots, interrupted


def is_live_frame(frame):
    """有没有一帧是「转轮行」：行首点阵字符。"""
    return any(line.lstrip() and line.lstrip()[0] in BRAILLE for line in frame)


def check_normal(raw, screen, snapshots):
    text = "\n".join(screen)
    report = {}
    report["no_inline_card"] = "×1" not in text and "↳" not in text
    report["no_worked_for"] = not is_fold_summary(text)
    report["reply_seen"] = "走查的回复" in text
    report["thought_line"] = any("已思考 ·" in line for line in screen)
    command_rows = [i for i, line in enumerate(screen)
                    if line.startswith("  $ ") and "运行命令" in line]
    # 09-17 起抬头给的是 **title**（「这条命令来干嘛的」），命令本身在下面几行。
    # 原来这儿断言的是「命令文本在抬头上」——那是改之前的样子，一直红着。
    report["command_line_has_title"] = any("跑个命令" in screen[i] for i in command_rows)
    report["command_text_under_step"] = any(
        "printf" in screen[i + 1] for i in command_rows if i + 1 < len(screen)
    )
    # 命令输出尾巴**紧贴**在那一行底下（不空行），每一行从连线穿过：`  │ 第二行`。
    tail_ok = False
    no_blank = False
    for i in command_rows:
        below = screen[i + 1:i + 6]
        if any(line.startswith("  │ ") and "第二行" in line for line in below):
            tail_ok = True
        no_blank = bool(screen[i + 1].strip())
    report["command_output_under_step"] = tail_ok
    report["command_output_has_no_blank_gap"] = no_blank
    edit_rows = [i for i, line in enumerate(screen) if "编辑文件" in line]
    diff_ok = False
    for i in edit_rows:
        below = screen[i + 1:i + 8]
        if any("+ 走查用的第一行" in line for line in below):
            diff_ok = True
    report["diff_under_edit_step"] = diff_ok
    # 报错那一步：标红 + 错误输出。
    raw_text = raw.decode("utf-8", "replace")
    # 同上：`exit 3` 现在在抬头**底下**那一行，不在抬头上。抬头要的是整行标红。
    report["failed_step_red"] = bool(re.search(r"\x1b\[31m[^\n]*运行命令[^\n]*", raw_text))
    report["failed_command_under_step"] = any(
        "exit 3" in line for line in screen
    )
    report["failed_step_shows_stderr"] = any("走查用的报错" in line for line in screen)
    # 步与步之间有连线。
    step_rows = [i for i, line in enumerate(screen) if STEP.match(line)]
    report["step_rows"] = len(step_rows)
    rails = sum(1 for line in screen if line.strip() == "│")
    report["rails_between_steps"] = rails >= max(0, len(step_rows) - 2)
    # 时间线整体退两格（用户：贴到左边框太靠左）。
    report["steps_indented_two"] = bool(step_rows) and all(
        screen[i].startswith("  ") and not screen[i].startswith("   ") for i in step_rows
    )
    # 连线贯穿：从第一步到最后一步之间，每一行要么是步骤行，要么以 `  │` 开头。
    if step_rows:
        span = screen[step_rows[0]:step_rows[-1] + 1]
        report["rail_continuous"] = all(
            STEP.match(line) or line.startswith("  │") for line in span
        )
    else:
        report["rail_continuous"] = False
    # 跑着的时候抓到过转轮行。
    live_frames = [frame for frame in snapshots if is_live_frame(frame)]
    report["live_spinner_seen"] = bool(live_frames)
    # 转轮行上面接的是 `│`（已经有步骤落地之后）。
    continued = False
    for frame in live_frames:
        for i, line in enumerate(frame):
            if line.lstrip() and line.lstrip()[0] in BRAILLE and i > 0 and frame[i - 1].strip() == "│":
                continued = True
    report["live_row_continues_the_rail"] = continued
    # 命令跑着的时候尾巴露在转轮行底下。
    tail_live = False
    for frame in live_frames:
        for i, line in enumerate(frame):
            if line.lstrip() and line.lstrip()[0] in BRAILLE and "运行命令" in line:
                below = frame[i + 1:i + 4]
                if any(l.startswith("  │ ") and ("走查用的命令输出" in l or "第二行" in l) for l in below):
                    tail_live = True
    report["command_tail_live"] = tail_live
    # 「展开思考内容」开着时，思考正文边想边落地：还在转（帧里有转轮）的时候，
    # 抬头「思考中」已经落地、底下已有正文行从连线穿过去。
    thought_live = False
    for frame in live_frames:
        heading = next((i for i, line in enumerate(frame) if "思考中" in line), None)
        if heading is None:
            continue
        below = frame[heading + 1:]
        if any(l.startswith("  │ ") and "先看一眼需求" in l for l in below):
            thought_live = True
    if os.environ.get("ST_EXPAND_REASONING"):
        report["thought_body_live"] = thought_live
    else:
        # 开关关着：正在想时抬头底下开一扇窗，滚着露最近几行（同一种 `  │` 行，
        # 用户 09-17：「思考行不是单行窥视，而是有滚动」）；想完只剩一行 tag。
        report["thought_window_live"] = thought_live
        report["thought_body_collapsed_when_done"] = not any(
            "先看一眼需求" in line for line in screen
        )
    # 时间线和正文之间空一行。
    reply_rows = [i for i, line in enumerate(screen) if "走查的回复" in line]
    gap_ok = False
    for i in reply_rows:
        j = i - 1
        while j >= 0 and not screen[j].strip():
            j -= 1
        gap_ok = i - j >= 2
    report["blank_before_reply"] = gap_ok
    return report, live_frames


def check_interrupt(raw, screen, snapshots, interrupted):
    text = "\n".join(screen)
    report = {}
    report["interrupt_sent"] = interrupted
    # `│ ` 现在是正文行的连线前缀，不再是 inline 卡片的记号；卡片认 `×1` 和 `↳`。
    report["no_inline_card"] = "×1" not in text and "↳" not in text
    report["interrupted_step"] = any("已中断" in line and "运行命令" in line for line in screen)
    raw_text = raw.decode("utf-8", "replace")
    report["interrupted_step_red"] = bool(re.search(r"\x1b\[31m[^\n]*运行命令[^\n]*已中断", raw_text))
    report["cancelled_notice"] = "已取消" in text
    return report


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
    OUT.mkdir(parents=True, exist_ok=True)
    for stale in OUT.glob("*.txt"):
        stale.unlink()
    write_config()
    kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_REASONING="1",
            STUB_TOOL="1",
            STUB_EDIT="1",
            STUB_FAIL="1",
            STUB_EDIT_PATH=str(EDIT_FILE),
            # 慢一点：转轮行和跑着时露出来的输出尾巴只有在它还跑着时才抓得到。
            STUB_TOOL_COMMAND=(
                "printf '走查用的命令输出\\n'; sleep 1.2; printf '第二行\\n'; sleep 1.2"
            ),
            # 带换行：「展开思考内容」下想完整行的正文要边想边落地。
            STUB_REASONING_TEXT="先看一眼需求，再决定怎么下手。\n" * 6,
        ),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = None
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

        # 第一轮：正常跑完。
        process, master = spawn_one_shot(PROMPT)
        raw, snapshots, _ = run_turn(process, master)
        screen = render(raw)
        (OUT / "raw-normal.bin").write_bytes(raw)
        (OUT / "screen-normal.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
        normal, live_frames = check_normal(raw, screen, snapshots)
        for index, frame in enumerate(live_frames[:6]):
            (OUT / f"live-{index}.txt").write_text("\n".join(frame) + "\n", encoding="utf-8")
        report["normal"] = normal

        # 第二轮：命令跑到一半 Ctrl+C。
        process, master = spawn_one_shot(PROMPT + " 再来一次")
        raw, snapshots, interrupted = run_turn(
            process, master,
            interrupt_when=lambda frame: any(
                line.lstrip() and line.lstrip()[0] in BRAILLE and "运行命令" in line for line in frame
            ),
        )
        screen = render(raw)
        (OUT / "raw-interrupt.bin").write_bytes(raw)
        (OUT / "screen-interrupt.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
        report["interrupt"] = check_interrupt(raw, screen, snapshots, interrupted)

        # 第三轮：提问面板。答完之后一问一答要融进时间线（连线穿过），而不是
        # 面板自己留一块「已回答」挂在抬头上面。桩模型要重起一份带 STUB_ASK 的。
        stub.send_signal(signal.SIGTERM)
        stub.wait(timeout=5)
        stub = subprocess.Popen(
            [sys.executable, str(SMOKE / "stub_llm.py")],
            env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_ASK="1", STUB_REASONING="1"),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        if not wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"):
            print("! 桩模型没起来（问答轮）", file=sys.stderr)
            return 2
        process, master = spawn_one_shot(PROMPT + " 再问一次")
        answered = {"sent": False}

        def answer_when_asked(frame):
            if not answered["sent"] and any("走查用的问题" in l for l in frame):
                answered["sent"] = True
                os.write(master, b"\r")
            return False

        raw, snapshots, _ = run_turn(process, master, interrupt_when=answer_when_asked)
        screen = render(raw)
        (OUT / "screen-ask.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
        ask = {}
        ask["answered"] = answered["sent"]
        step = next((i for i, l in enumerate(screen) if "询问用户" in l and "已回答" in l), None)
        ask["ask_step_present"] = step is not None
        if step is not None:
            below = screen[step + 1:step + 6]
            ask["answers_under_step_with_rail"] = (
                any(l.startswith("  │") and "已回答 1 个问题" in l for l in below)
                and any(l.startswith("  │") and "走查：" in l for l in below)
            )
            # 面板自己那块（带 `┃`）不该留下。
            ask["no_panel_residue"] = not any("┃" in l for l in screen)
        report["ask"] = ask

        (OUT / "report.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        print(json.dumps(report, ensure_ascii=False, indent=2))
        bad = [
            f"{section}.{key}"
            for section, items in report.items()
            for key, value in items.items()
            if value is False
        ]
        if bad:
            print("! 不过：" + ", ".join(bad), file=sys.stderr)
            return 1
        return 0
    finally:
        for process in (daemon, stub):
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


if __name__ == "__main__":
    sys.exit(main())
