#!/usr/bin/env python3
"""任务清单表在全屏 TUI 里的两个毛病:被 `Worked for` 截断、要等整条时间线跑完
才出现。

用 tmux 当终端仿真器(不用 pyte),桩模型按 todowrite → run_command → 正文的顺序
走一轮,期间连续抓屏:

- 表出现的时刻 vs 第一条 `Worked for` 出现的时刻 → 「等到最后才出」
- 终屏里表格边框是否完整、下面紧跟的是不是 `Worked for` → 「被截断」

跑法:
    python3 testkit/tui/todo_table.py [--binary /abs/path/to/miyu]
产物在 ~/.cache/miyu-todo-table/
"""

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SMOKE = ROOT / "testkit" / "repl-smoke"
SESSION = "miyutodo"
PORT = int(os.environ.get("STUB_PORT", "18497"))
HOME = Path("/tmp/miyu-todo-table/home")
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-todo-table"))
COLS = int(os.environ.get("COLS", "120"))
ROWS = int(os.environ.get("ROWS", "45"))


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps({
            "oobe_done": True,
            "active_provider": "stub",
            "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
            "providers": [{
                "id": "stub", "display_name": "Stub",
                "base_url": f"http://127.0.0.1:{PORT}/v1",
                "protocol": "openai-chat", "api_key": "stub",
                "models": ["stub-model"],
            }],
            "display": {"language": "zh"},
            "memory": {"enabled": False},
        }, ensure_ascii=False, indent=2),
        encoding="utf-8",
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


def capture():
    return subprocess.run(
        ["tmux", "capture-pane", "-pt", SESSION]
        + (["-e"] if os.environ.get("KEEP_ANSI") else []),
        capture_output=True, text=True, check=False,
    ).stdout.splitlines()


def require_tmux():
    """这份走查拿 tmux 当终端仿真器。没装的话原来是一句
    `FileNotFoundError: 'tmux'`,看起来像被测的东西坏了——实际是机器上缺件。"""
    if shutil.which("tmux"):
        return True
    print("! 这份走查要 tmux(拿它当终端仿真器),这台机器上没有。\n"
          "  Linux: 用你的包管理器装 tmux;macOS: brew install tmux。",
          file=sys.stderr)
    return False


def belongs_to_sandbox(pid):
    """这个进程是不是**我们这轮**的沙箱 daemon。

    Linux 上直接读它的环境最准。macOS 读不到:SIP 之后连同用户的进程都不给,
    `ps -E`/`ps eww` 打出来只有命令行(09-23 在真机上验过)。退而求其次看它开着
    哪些文件——daemon 一定开着沙箱 HOME 底下的库。`/tmp` 在 macOS 上是
    `/private/tmp` 的符号链接,所以两种写法都要比。"""
    try:
        env = pathlib.Path(f"/proc/{pid}/environ").read_bytes().decode("utf-8", "replace")
        return f"MIYU_HOME={HOME}" in env
    except OSError:
        pass
    if not shutil.which("lsof"):
        return False
    out = subprocess.run(["lsof", "-p", str(pid), "-Fn"],
                         capture_output=True, text=True, check=False).stdout
    return str(HOME) in out or os.path.realpath(HOME) in out


def kill_sandbox_daemon():
    """上一轮的沙箱 daemon 会活过 tmux 会话:新 TUI 连上它就拿到旧会话历史,
    桩按「已有几条工具结果」定档,于是直接跳到收尾那一档——看起来像工具没调。"""
    out = subprocess.run(["pgrep", "-f", "__daemon"], capture_output=True, text=True,
                         check=False).stdout.split()
    for pid in out:
        if belongs_to_sandbox(pid):
            subprocess.run(["kill", pid], check=False)
    time.sleep(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(ROOT / "target" / "debug" / "miyu"))
    args = parser.parse_args()
    binary = Path(args.binary).resolve()

    if not require_tmux():
        return 2

    kill_sandbox_daemon()
    shutil.rmtree(HOME.parent, ignore_errors=True)
    OUT.mkdir(parents=True, exist_ok=True)
    write_config()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(PORT), STUB_TODO=os.environ.get("TODO", "1"), STUB_TOOL=os.environ.get("TOOL", "1"),
                 STUB_REASONING=os.environ.get("REASONING", "1"), STUB_CHUNK_SLEEP="0.05",
                 **({"STUB_STAGE_PREFACE": "1"} if os.environ.get("PREFACE") else {}),
                 STUB_TOOL_ROUNDS=os.environ.get("TOOL_ROUNDS", "1"),
                 STUB_RESPONSE_DELAY=os.environ.get("RESPONSE_DELAY", "0"),
                 **({"STUB_TODO_REPEAT": "1"} if os.environ.get("TODO_REPEAT") else {})),
        stdout=(OUT / "stub.log").open("w"), stderr=subprocess.STDOUT,
    )
    try:
        if not wait_http(f"http://127.0.0.1:{PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 1
        subprocess.run(["tmux", "kill-session", "-t", SESSION],
                       capture_output=True, check=False)
        subprocess.run(
            ["tmux", "new-session", "-d", "-s", SESSION, "-x", str(COLS), "-y", str(ROWS),
             f"MIYU_HOME={HOME} TERM=xterm-256color MIYU_TUI={os.environ.get('MIYU_TUI', '1')} {binary}"],
            check=True,
        )
        time.sleep(9)
        subprocess.run(["tmux", "send-keys", "-t", SESSION, "-l", "走查一句"], check=True)
        time.sleep(1)
        subprocess.run(["tmux", "send-keys", "-t", SESSION, "Enter"], check=True)

        timeline_ms = None
        table_ms = None
        started = time.time()
        frames = []
        for _ in range(60):
            time.sleep(0.5)
            screen = capture()
            blob = "\n".join(screen)
            elapsed = int((time.time() - started) * 1000)
            if timeline_ms is None and ("Worked for" in blob or "干了" in blob):
                timeline_ms = elapsed
            if table_ms is None and "[✔]" in blob:
                table_ms = elapsed
            frames.append((elapsed, blob))
            if timeline_ms and table_ms and elapsed - max(timeline_ms, table_ms) > 4000:
                break

        if os.environ.get("CLICK"):
            # 点开第一条 `Worked for`:展开会把块区换成整条时间线,后面的内容
            # 要跟着挪。表是写在块**外面**的裸文本——它挪得对不对就在这一下。
            screen = capture()
            row = next((i for i, line in enumerate(screen) if "Worked for" in line), None)
            if row is not None:
                col = screen[row].index("›") + 2
                for final_byte in ("M", "m"):
                    seq = f"\x1b[<0;{col + 1};{row + 1}{final_byte}"
                    subprocess.run(["tmux", "send-keys", "-t", SESSION, "-H",
                                    *[f"{byte:02x}" for byte in seq.encode()]], check=False)
                    time.sleep(0.3)
                time.sleep(1.5)
                (OUT / "after-click.txt").write_text("\n".join(capture()), encoding="utf-8")
                if os.environ.get("CLICK") == "2":
                    # 再点一次收起。用户原话是「被 `Worked for` **缩起**截断」——
                    # 收起时块区要从整条时间线缩回一行,下面那张裸表得跟着上移。
                    screen2 = capture()
                    row2 = next((i for i, line in enumerate(screen2) if "Worked for" in line), row)
                    col2 = screen2[row2].index("⌄") + 2 if "⌄" in screen2[row2] else col
                    for final_byte in ("M", "m"):
                        seq = f"\x1b[<0;{col2 + 1};{row2 + 1}{final_byte}"
                        subprocess.run(["tmux", "send-keys", "-t", SESSION, "-H",
                                        *[f"{byte:02x}" for byte in seq.encode()]], check=False)
                        time.sleep(0.3)
                    time.sleep(1.5)
                    (OUT / "after-collapse.txt").write_text("\n".join(capture()), encoding="utf-8")
                    print("(又点了一次收起)")
                print(f"(点了第 {row + 1} 行的 Worked for)")

        final = capture()
        (OUT / "screen.txt").write_text("\n".join(final), encoding="utf-8")
        (OUT / "frames.txt").write_text(
            "\n\n".join(f"=== +{ms}ms ===\n{blob}" for ms, blob in frames), encoding="utf-8"
        )

        print(f"首次出现 `Worked for`: {timeline_ms} ms")
        print(f"首次出现 表格行 `[✔]`: {table_ms} ms")
        if timeline_ms is not None and table_ms is not None:
            print("判定: " + ("表在时间线收缩行之后才出现(现状)"
                              if table_ms >= timeline_ms else "表先于收缩行出现"))
        print("\n--- 终屏里表格那一段 ---")
        for index, line in enumerate(final):
            if "[✔]" in line:
                for row in final[max(0, index - 2): index + 10]:
                    print(row)
                break
        else:
            print("(终屏里没有表)")
        print(f"\n产物: {OUT}")
        return 0
    finally:
        subprocess.run(["tmux", "kill-session", "-t", SESSION],
                       capture_output=True, check=False)
        stub.terminate()


if __name__ == "__main__":
    sys.exit(main())
