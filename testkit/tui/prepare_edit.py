"""「准备编辑」那一段的走查:模型在流一次 `edit` 的补丁参数时,屏上长什么样。

桩模型先吐工具名、再把补丁分片慢慢吐(`STUB_TOOL_ARG_CHUNK` + `STUB_CHUNK_SLEEP`),
拉出几秒「准备编辑」的窗口。从发出去开始每 0.25 秒抓一屏,不一样的屏都落盘,
好对着看每一帧。

    cargo build
    python3 testkit/tui/prepare_edit.py

产物在 ~/.cache/miyu-prepare-edit/(frames/*.txt、frames.jsonl、raw.bin)。
**这些 TUI 走查只能一个一个跑**:共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import round26 as r  # noqa: E402
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-prepare-edit"))


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    (OUT / "frames").mkdir(parents=True)
    stub_env = {
        "STUB_EDIT": "1",
        "STUB_EDIT_PATH": str(h.EDIT_FILE),
        "STUB_TOOL_ARG_CHUNK": os.environ.get("ARG_CHUNK", "4"),
        "STUB_CHUNK_SLEEP": os.environ.get("CHUNK_SLEEP", "0.12"),
    }
    # 额外的桩开关(场景):STUB_EXTRA='{"STUB_TOOL":"1","STUB_STAGE_PREFACE":"1"}'。
    stub_env.update(json.loads(os.environ.get("STUB_EXTRA", "{}")))
    stub, daemon, tui, master, sink = r.start(stub_env)
    frames = []
    try:
        os.write(master, "改一下那个文件".encode())
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        last = None
        started = time.time()
        while time.time() - started < float(os.environ.get("WATCH", "30")):
            h.drain(master, 0.25, sink)
            screen = h.render(bytes(sink))
            if screen != last:
                index = len(frames)
                (OUT / "frames" / f"{index:03d}.txt").write_text("\n".join(screen), encoding="utf-8")
                interesting = [line for line in screen if any(
                    mark in line for mark in ("准备", "编辑", "Preparing", "edit", "补丁"))]
                frames.append({"t": round(time.time() - started, 2), "index": index, "lines": interesting})
                last = screen
            if any("好的" in line or "收到" in line for line in screen) and time.time() - started > 3:
                h.drain(master, 1.5, sink)
                break
        (OUT / "raw.bin").write_bytes(bytes(sink))
        with (OUT / "frames.jsonl").open("w", encoding="utf-8") as out:
            for frame in frames:
                out.write(json.dumps(frame, ensure_ascii=False) + "\n")
        for frame in frames:
            print(f"[{frame['t']:5.2f}s #{frame['index']:03d}]", " | ".join(line.strip() for line in frame["lines"])[:220])
    finally:
        r.stop(tui, daemon, stub)
    print(f"产物：{OUT}")


if __name__ == "__main__":
    main()
