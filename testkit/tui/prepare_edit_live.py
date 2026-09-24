#!/usr/bin/env python3
"""「准备编辑」的**真模型**探针:真供应商吐 edit 参数的节奏,屏上看不看得到那一行。

桩模型把工具名先吐、参数分片慢吐,「准备编辑」总看得见;真供应商未必——有的把
工具名和整段参数一口气给完,准备窗口只有一瞬。这里借真配置里的一条线路(连 key,
沙箱 home 与 live.py 同一套,不碰 ~/.miyu),让它用 edit 写一段长内容,每 0.2 秒抓一屏。

    LIVE_PROVIDER=opencodego LIVE_MODEL=deepseek-v4.1-flash python3 testkit/tui/prepare_edit_live.py

产物在 ~/.cache/miyu-prepare-edit-live/<供应商>-<模型>/(frames.jsonl、raw.bin)。
frames.jsonl 是每 0.2 秒一帧的状态:思考中 / 已思考 / 只剩转轮 / 准备编辑 / 编辑文件。
把工具调用扣着不放的线路上,思考停了 1.5s 起应当只剩转轮(`timeline/stall.rs`)。
"""

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import live as L  # noqa: E402

# live.py 默认用 release;探针跟着 debug 构建走(MIYU_BIN 可换)。
L.BIN = Path(os.environ.get("MIYU_BIN", L.ROOT / "target" / "debug" / "miyu"))

BRAILLE = set("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-prepare-edit-live")) / f"{L.PROVIDER}-{L.MODEL}".replace("/", "_")
TASKS = [
    "用 edit 工具新建 /tmp/miyu-tui-live/work/story.md,写一段 30 行的小说开头,每行一句话。别用命令行,也别先读目录,直接写。",
    "用 edit 工具把 story.md 的第 3 行改成「雨下了一整夜」,别用命令行。",
]


def lone_spinner(line):
    """只有转轮(和连线)的一行:没有字。"""
    stripped = line.strip()
    return bool(stripped) and stripped[0] in BRAILLE and set(stripped[1:]) <= set(" │")


def state(screen):
    thought = [line.strip() for line in screen if "已思考" in line]
    return {
        "thinking": any("思考中" in line for line in screen),
        "thought": thought[-1] if thought else None,
        "lone": any(lone_spinner(line) for line in screen),
        "prepare": any("准备编辑" in line for line in screen),
        "edit": any("编辑文件" in line for line in screen),
    }


def main():
    if L.HOME.exists():
        shutil.rmtree(L.HOME)
    Path(L.RUNTIME).mkdir(exist_ok=True)
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    L.write_config()
    daemon = subprocess.Popen(
        [str(L.BIN), "__daemon", "--port", str(L.PORT)],
        env=dict(os.environ, MIYU_HOME=str(L.HOME), XDG_RUNTIME_DIR=L.RUNTIME),
        cwd=str(L.WORKSPACE), stdin=subprocess.DEVNULL,
        stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
    )
    if not L.wait_http(f"http://127.0.0.1:{L.PORT}/api/config", timeout=60):
        daemon.terminate()
        print("! daemon 没起来", file=sys.stderr)
        return 2
    process, master = L.spawn()
    sink = bytearray()
    report = []
    try:
        L.drain(master, 3.0, sink)
        frames = (OUT / "frames.jsonl").open("w", encoding="utf-8")
        for task in TASKS:
            os.write(master, task.encode())
            L.drain(master, 0.4, sink)
            os.write(master, b"\r")
            started = time.time()
            seen_prepare = None
            prepare_last = None
            seen_edit = None
            quiet_since = None
            last_screen = None
            while time.time() - started < 180:
                L.drain(master, 0.2, sink)
                screen = L.render(bytes(sink))
                now = round(time.time() - started, 2)
                frames.write(json.dumps({"task": task[:12], "t": now, **state(screen)}, ensure_ascii=False) + "\n")
                prep = [line.strip() for line in screen if "准备编辑" in line or "Preparing edit" in line]
                if prep:
                    seen_prepare = seen_prepare if seen_prepare is not None else now
                    prepare_last = (now, prep[-1])
                if seen_edit is None and any("编辑文件" in line for line in screen):
                    seen_edit = now
                if screen == last_screen:
                    quiet_since = quiet_since or time.time()
                    if seen_edit is not None and time.time() - quiet_since > 3.0:
                        break
                else:
                    quiet_since = None
                last_screen = screen
            entry = {"task": task[:24], "prepare_first": seen_prepare, "prepare_last": prepare_last,
                     "edit_step_at": seen_edit}
            report.append(entry)
            frames.flush()
            print(json.dumps(entry, ensure_ascii=False))
        (OUT / "raw.bin").write_bytes(bytes(sink))
        (OUT / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    finally:
        for proc in (process, daemon):
            try:
                proc.terminate()
                proc.wait(5)
            except Exception:
                proc.kill()
    print(f"产物：{OUT}")


if __name__ == "__main__":
    sys.exit(main() or 0)
