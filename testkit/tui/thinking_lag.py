#!/usr/bin/env python3
"""量尺：思考正文越来越长时，TUI 还跟不跟得上。

桩模型把一大段思考（默认 150KB）按小块慢慢吐，模拟真模型想了几分钟、几万词元。每隔
几秒敲一个字量回显，顺带报 TUI 进程自己的 CPU。用户 09-24：「思考中词元越来越大，
TUI 就会变得超级卡，几乎没法交互」——修前（转轮每一拍整段重折）30k 词元时 97% CPU、
回显 2–4 秒；修后（`timeline::ThoughtRows` 只往后补）46k 词元时约 5%、10ms 上下。

只报数、不设通过线：耗时跟机器走。对比修前修后用 `MIYU_BIN` 换二进制。

    cargo build
    python3 testkit/tui/thinking_lag.py [总字节 150000] [吐完用几秒 60] [--one-paragraph]

`--one-paragraph`：整段不换行（最坏情况：最后那半行一直在长）。
"""

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402,F401  (导入时剥掉 HERDR_*)
import round26 as r  # noqa: E402
import latency_probe as lp  # noqa: E402

args = [arg for arg in sys.argv[1:] if not arg.startswith("--")]
ONE_PARAGRAPH = "--one-paragraph" in sys.argv
TOTAL = int(args[0]) if len(args) > 0 else 150_000
SECONDS = float(args[1]) if len(args) > 1 else 60.0
PARAGRAPH = (
    "先把这一段想清楚再动手：setLeg(true, theta); setLeg(false, theta + Math.PI);"
    " 车头轻微摆动，整架上下微颠加左右微倾。"
) + ("" if ONE_PARAGRAPH else "\n")
TEXT = PARAGRAPH * (TOTAL // len(PARAGRAPH.encode()) + 1)
CHUNK_CHARS = 40


def cpu_ticks(pid):
    try:
        fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])
    except (OSError, IndexError, ValueError):
        return None


def main():
    chunks = max(1, len(TEXT) // CHUNK_CHARS)
    stub_env = {
        "STUB_REASONING": "1",
        "STUB_REASONING_TEXT": TEXT,
        "STUB_CHUNK_CHARS": str(CHUNK_CHARS),
        "STUB_CHUNK_SLEEP": f"{SECONDS / chunks:.4f}",
    }
    # `round26.start` 自己起桩模型；这段思考超过 64KB 要走文件（`latency_probe.start_stub`），
    # 先起一个空的再换掉。
    stub, daemon, tui, master, sink = r.start({})
    lp.stop(stub)
    stub = lp.start_stub(stub_env)
    tty = lp.Tty(master)
    tty.stream.feed(bytes(sink))
    rows = []
    try:
        tty.settle(1.0)
        tty.send("想很久很久\r".encode())
        started = time.time()
        tty.wait_since(started, lambda text: "思考中" in text, 20)
        last_cpu, last_at = cpu_ticks(tui.pid), time.time()
        while time.time() - started < SECONDS + 5:
            tty.settle(4.0)
            now = time.time()
            cpu = cpu_ticks(tui.pid)
            cpu_pct = None
            if cpu is not None and last_cpu is not None:
                cpu_pct = round((cpu - last_cpu) / os.sysconf("SC_CLK_TCK") / (now - last_at) * 100, 1)
            last_cpu, last_at = cpu, now
            typed = time.time()
            tty.send(b"z")
            echo = tty.wait_since(typed, lambda text: "┃ z" in text, 10)
            tty.send(b"\x7f")
            head = next((line.strip() for line in tty.text().split("\n") if "思考中" in line), "")
            rows.append({"t": round(now - started, 1), "echo_ms": echo, "tui_cpu_pct": cpu_pct,
                         "head": head[:40]})
            print(json.dumps(rows[-1], ensure_ascii=False), flush=True)
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    main()
