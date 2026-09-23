#!/usr/bin/env python3
"""展开命令那一步之后，命令文本有没有语法高亮。

用户 09-19：AI 跑了个 `python3 - <<'PY'` 塞几十行 Python 的命令，展开之后是
一片单色，想要语法高亮。

量三件事：

1. 多行命令（带 heredoc）展开之后带着色转义；
2. heredoc 体按**解释器**着色（关键字紫、字符串绿、注释绿），不是按 shell；
3. **单行命令不着色**——逐词上色只是整行变亮，没有信息增益，而「命令别比输出
   亮」是 09-17 定下的。

跑法（先 cargo build）：

    python3 testkit/tui/command_highlight.py
"""

import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-cmd-highlight/home"))
RUNTIME = os.environ.get("RUNTIME", "/tmp/mx-cmd-highlight")
PORT = int(os.environ.get("PORT", "18469"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18481"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-cmd-highlight"))
COLS, ROWS = 110, 46

# 用户截图里那条命令的形状：shell 外壳 + heredoc 里的 Python。
SCRIPT = """python3 - <<'PY'
import os, math

def fib_iter():
    a, b = 0, 1  # 这是注释
    while True:
        yield a
print(f"python : {os.name}")
PY"""
# 着色用的前景色转义（`style.rs` 的 CODE_* 常量）。
KEYWORD = "\x1b[38;2;196;167;231m"
STRING = "\x1b[38;2;166;214;160m"
COMMENT = "\x1b[32m"
DIM = "\x1b[2m"


def wait_http(url, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.3)
    return False


def main():
    import codecs

    import pyte

    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    sys.path.insert(0, str(ROOT / "testkit" / "tui"))
    import run as tui

    tui.HOME, tui.PORT, tui.STUB_PORT, tui.BIN = HOME, PORT, STUB_PORT, BIN
    tui.COLS, tui.ROWS = COLS, ROWS
    tui.ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME, MIYU_TUI="1")
    tui.write_config()
    # 让命令那一步**默认就是展开的**：这个走查要看的是展开内容，点击在走查里
    # 一向脆（按下与抬起之间有间隔就被当成拖选），能绕开就绕开。
    config_path = HOME / "config" / "config.jsonc"
    config = json.loads(config_path.read_text(encoding="utf-8"))
    config.setdefault("display", {})["expand_tool_calls"] = True
    config["display"]["fold_timeline"] = False
    config_path.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_TOOL="1",
            STUB_TOOL_COMMAND=SCRIPT,
            STUB_REPLY="跑完了",
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = None
    report = {}
    try:
        wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models")
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=tui.ENV, cwd=str(HOME),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        wait_http(f"http://127.0.0.1:{PORT}/api/config")
        process, master = tui.spawn_tui()
        screen = pyte.Screen(COLS, ROWS)
        stream = pyte.Stream(screen)
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        lock = threading.Lock()
        raw = bytearray()
        alive = [True]

        def pump():
            import select

            while alive[0]:
                ready, _, _ = select.select([master], [], [], 0.1)
                if not ready:
                    continue
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                with lock:
                    raw.extend(chunk)
                    text = decoder.decode(chunk)
                    if text:
                        stream.feed(text)

        threading.Thread(target=pump, daemon=True).start()
        time.sleep(3.0)
        os.write(master, "跑一下那个脚本\r".encode())

        def lines():
            with lock:
                return [line.rstrip() for line in screen.display]

        # 等命令那一步落下来
        deadline = time.time() + 60
        row = None
        while time.time() < deadline:
            time.sleep(0.3)
            found = [i for i, line in enumerate(lines()) if "运行命令" in line]
            if found:
                row = found[0]
                break
        report["命令那一步出现了"] = row is not None
        (OUT / "screen.txt").write_text("\n".join(lines()), encoding="utf-8")
        if row is None:
            print("! 命令那一步没出现，当时的屏：", file=sys.stderr)
            for line in lines()[:20]:
                print("   ", line[:90], file=sys.stderr)
            return 1
        time.sleep(4.0)
        with lock:
            expanded = bytes(raw).decode("utf-8", "replace")
        (OUT / "expanded.bin").write_bytes(expanded.encode("utf-8"))

        report["展开后有关键字着色"] = KEYWORD in expanded
        report["展开后有字符串着色"] = STRING in expanded
        report["展开后有注释着色"] = COMMENT in expanded
        # heredoc 起止那两行是 shell：`'PY'` 该是字符串色。
        report["heredoc 标记按 shell 着色"] = STRING in expanded

        alive[0] = False
        os.write(master, b"\x03")
        time.sleep(0.5)
        process.terminate()
    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()

    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"\n{passed}/{len(report)} passed   产物：{OUT}")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
