#!/usr/bin/env python3
"""一个 TUI、一段慢回复：正文是**边流边出**还是**跑完才出**。

两视图走查里量到 A 的正文在整轮结束那一刻才整段冒出来。那要么是桩模型
（3 字一块、0.35 秒一块）把日常看不见的聚合行为放大了，要么是真的没在流。
这个脚本只开一个 TUI，每两秒记一次「屏上看得见的最大段号」，一眼看出来。

跑法（先 cargo build）：

    python3 testkit/tui/stream_timing.py
"""

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
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-stream-timing/home"))
RUNTIME = os.environ.get("RUNTIME", "/tmp/mx-stream-timing")
PORT = int(os.environ.get("PORT", "18463"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18487"))
COLS, ROWS = 100, 40
SEGMENTS = int(os.environ.get("SEGMENTS", "40"))
# 正文是**按行**落地的：一整段没有换行的长文本会被攒到收尾才出现（09-19
# 我拿它当「不流式」的证据，是测试数据不对）。真回复是分段的，所以这里也分段。
NEWLINE_EVERY = int(os.environ.get("NEWLINE_EVERY", "4"))
REPLY = "".join(
    f"这是第{i}段回复正文。" + ("\n\n" if i % NEWLINE_EVERY == 0 else "")
    for i in range(1, SEGMENTS + 1)
)


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


def ensure_my_stub(port, sentinel):
    """确认在跟**自己起的**桩说话。

    端口被别人占着时 `bind` 会失败，而探针只会看到一个「能应答的」服务——
    于是拿着别人的回复跑完全程，量到的一切都是假的（09-19 撞到另一个会话
    残留的桩，白跑两轮）。所以发一句话，看回的正文里有没有自己的暗号。
    """
    import json
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=json.dumps({
            "model": "stub", "stream": True,
            "messages": [{"role": "user", "content": "ping"}],
        }).encode(),
        headers={"content-type": "application/json"},
    )
    try:
        said = []
        with urllib.request.urlopen(request, timeout=30) as response:
            for line in response:
                if not line.startswith(b"data:"):
                    continue
                payload = line[5:].strip()
                if payload == b"[DONE]":
                    break
                try:
                    delta = json.loads(payload)["choices"][0]["delta"]
                except Exception:
                    continue
                said.append(str(delta.get("content") or ""))
    except Exception as error:
        print(f"! 桩模型探活失败：{error}", file=sys.stderr)
        return False
    # 暗号要在**拼起来**的正文里找：SSE 是几个字一块发的，原始响应里那串字
    # 是被切开的（09-19 第一版就栽在这儿）。
    if sentinel in "".join(said):
        return True
    print(
        f"! {port} 上的不是我起的桩（回的正文里没有暗号 {sentinel!r}）——"
        "多半是别的进程占着这个端口，换一个再跑",
        file=sys.stderr,
    )
    return False


def main():
    import codecs

    import pyte

    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    sys.path.insert(0, str(ROOT / "testkit" / "tui"))
    import run as tui

    tui.HOME, tui.PORT, tui.STUB_PORT, tui.BIN = HOME, PORT, STUB_PORT, BIN
    tui.COLS, tui.ROWS = COLS, ROWS
    tui.ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME, MIYU_TUI="1")
    tui.write_config()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_REASONING="1",
            STUB_REPLY=REPLY,
            STUB_CHUNK_SLEEP=os.environ.get("CHUNK_SLEEP", "0.35"),
            STUB_CHUNK_CHARS=os.environ.get("CHUNK_CHARS", "3"),
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = None
    try:
        wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models")
        if not ensure_my_stub(STUB_PORT, "这是第1段回复正文"):
            return 2
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
                    text = decoder.decode(chunk)
                    if text:
                        stream.feed(text)

        threading.Thread(target=pump, daemon=True).start()
        time.sleep(3.0)
        os.write(master, "说一段长的\r".encode())
        start = time.time()
        print(f"回复 {SEGMENTS} 段 / {len(REPLY)} 字，桩模型 3 字一块、0.35 秒一块")
        print("  秒   屏上最大段号")
        seen = []
        while time.time() - start < float(os.environ.get("WATCH", "160")):
            time.sleep(2.0)
            with lock:
                flat = re.sub(r"\s+", "", "\n".join(screen.display))
            found = [int(m) for m in re.findall(r"这是第(\d+)段回复正文", flat)]
            top = max(found) if found else None
            seen.append(top)
            with lock:
                rows = [line.rstrip() for line in screen.display]
            body = [line for line in rows[:34] if line.strip()]
            print(f"  {time.time() - start:4.0f}  {top}   正文区末行: {body[-1][:44] if body else '(空)'}")
            if top == SEGMENTS:
                break
        with lock:
            dump = "\n".join(line.rstrip() for line in screen.display)
        out = Path("/tmp/miyu-stream-timing/screen.txt")
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(dump, encoding="utf-8")
        print("屏幕 dump:", out)
        alive[0] = False
        os.write(master, b"\x03")
        time.sleep(0.5)
        process.terminate()
        progressive = len({s for s in seen if s is not None}) > 1
        print()
        print("判定：", "边流边出" if progressive else "**跑完才出**")
    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
