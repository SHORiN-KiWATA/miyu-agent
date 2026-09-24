"""思考停了、线路还扣着工具调用的那段静默：屏上只剩转轮（09-24）。

有的线路把整段工具调用扣着、模型写完才放（opencodego 的 deepseek-v4.1-flash
静默 1.5–3.9s，bigmodel 的 glm-5.3-flash 12s）。那段时间屏上原来一直是「思考中」
在走秒，「准备编辑」只闪几十毫秒。桩模型照这个节奏来：想两句，静默
`GAP` 秒，再把 edit 的参数分片慢吐。

判定：
  静默刚开始（<1.5s）照旧是「思考中」
  静默过了 1.5s：没有「思考中」、有「已思考」，live 区最后一行只剩转轮
  「已思考」的秒数不含静默
  工具名一到接上「准备编辑」，随后「编辑文件」
  收缩行的秒数把静默也算进去

    cargo build
    python3 testkit/tui/reasoning_stall.py

产物在 ~/.cache/miyu-reasoning-stall/（frames.jsonl、raw.bin）。
**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import re
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import round26 as r  # noqa: E402
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-reasoning-stall"))
GAP = float(os.environ.get("GAP", "4"))
STALL = 1.5
THOUGHT_RE = re.compile(r"已思考 · (?:\d+ 词元 · )?([\d.]+)(ms|s)")
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}" + (f"  ({detail})" if detail and not ok else ""))


def lone_spinner(line):
    """只有转轮（和连线）的一行：没有字。"""
    stripped = line.strip()
    return bool(stripped) and stripped[0] in r.BRAILLE and set(stripped[1:]) <= set(" │")


def seconds_of(value, unit):
    return float(value) / (1000 if unit == "ms" else 1)


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    stub_env = {
        "STUB_EDIT": "1",
        "STUB_EDIT_PATH": str(h.EDIT_FILE),
        "STUB_REASONING": "1",
        "STUB_TOOL_GAP": str(GAP),
        "STUB_TOOL_ARG_CHUNK": "4",
        "STUB_CHUNK_SLEEP": "0.12",
    }
    stub, daemon, tui, master, sink = r.start(stub_env)
    samples = []
    try:
        os.write(master, "改一下那个文件".encode())
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        started = time.time()
        while time.time() - started < GAP + 20:
            h.drain(master, 0.1, sink)
            screen = h.render(bytes(sink))
            at = round(time.time() - started, 2)
            thought = next((m for line in screen for m in [THOUGHT_RE.search(line)] if m), None)
            samples.append({
                "t": at,
                "thinking": any("思考中" in line for line in screen),
                "thought": thought.group(0) if thought else None,
                "thought_s": seconds_of(*thought.groups()) if thought else None,
                "lone": any(lone_spinner(line) for line in screen),
                "prepare": any("准备编辑" in line for line in screen),
                "edit": any("编辑文件" in line for line in screen),
                "fold": next((line.strip() for line in screen if h.is_fold_summary(line)), None),
            })
            if samples[-1]["fold"] and any("好的" in line for line in screen):
                break
        (OUT / "raw.bin").write_bytes(bytes(sink))
        with (OUT / "frames.jsonl").open("w", encoding="utf-8") as out:
            for sample in samples:
                out.write(json.dumps(sample, ensure_ascii=False) + "\n")
    finally:
        r.stop(tui, daemon, stub)

    thinking = [s["t"] for s in samples if s["thinking"] and not s["prepare"] and not s["edit"]]
    settled = [s for s in samples if s["thought"] and not s["thinking"] and not s["prepare"] and not s["edit"]]
    check("想的时候是「思考中」", thinking, "一帧都没抓到")
    check("静默过了 1.5s 收成「已思考」", settled, "没有只剩「已思考」的帧")
    if thinking and settled:
        waited = settled[0]["t"] - thinking[-1]
        # 抓屏每 0.1s 一帧，思考中最后一帧与收尾之间最多再差一帧。
        check("收尾不早于静默 1.5s（中途短停不收）", thinking[-1] >= 1.0, f"思考中最后一帧 {thinking[-1]}s")
        check("收尾没拖太久", waited <= 0.6, f"思考中最后一帧→收尾 {round(waited, 2)}s")
    check("收尾之后 live 区只剩转轮", settled and all(s["lone"] for s in settled),
          str([s["t"] for s in settled if not s["lone"]][:5]))
    check("「已思考」的秒数不含静默", settled and settled[0]["thought_s"] < STALL,
          settled[0]["thought"] if settled else "")
    prepare = [s["t"] for s in samples if s["prepare"]]
    check("工具名一到接上「准备编辑」", prepare and settled and prepare[0] > settled[0]["t"], str(prepare[:3]))
    check("随后「编辑文件」", any(s["edit"] for s in samples))
    fold = next((s["fold"] for s in reversed(samples) if s["fold"]), None)
    match = re.search(r"(?:Worked for |· )([\d.]+)s", fold or "")
    check("收缩行的秒数把静默也算进去", match and float(match.group(1)) >= GAP, fold or "没有收缩行")
    print(f"{sum(results)}/{len(results)} passed    产物：{OUT}")
    sys.exit(0 if results and all(results) else 1)


if __name__ == "__main__":
    main()
