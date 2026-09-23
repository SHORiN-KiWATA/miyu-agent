#!/usr/bin/env python3
"""后台子代理的过程**多久才到一次面板**——给阶段 4（路 B）拍板用的那个数。

`docs/plan/2026-09-17-render-unification.md` §6.1 给了两条路：

- 路 A（已做）：后台面板仍然读日志，只是解码换成共用的 `LogEvent`。
- 路 B（待拍板）：加一条 IPC `Command::JobTrace`，面板直接订原始标记流。

报告给路 B 写了「不做的条件」：**如果实测段落级刷新用户不介意，路 B 的收益只剩
去掉 150ms 轮询，不值一条新 IPC**。那就得先有「段落级刷新到底多卡」这个数。

卡在哪：内层的思考/正文是**按自然段**落盘的（`tools/subagent.rs::accumulate_stream`
——遇到空行、或攒够 600 字符才落一条），落盘之后面板还要等下一次 150ms 轮询。
模型想一大段不带空行的时候，这两段延迟是叠加的。

这份走查用桩模型派一个后台子代理，让它的思考**不带空行**，然后：

- 记下桩每发一块的时刻（它自己按 `STUB_CHUNK_SLEEP` 匀速发）；
- 每 20ms 看一次任务日志，记下每条 `[思考]` 落盘的时刻与长度。

出来的是「一条思考要憋多久才露面」的分布。跑法：

    cargo build
    python3 testkit/tui/bg_latency.py

产物：~/.cache/miyu-bg-latency/report.json
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path.home() / ".cache" / "miyu-bg-latency"

# 一段不带空行的思考：`accumulate_stream` 只有攒够 600 字符才肯落一条。
# 真实模型想一大段时就是这个样子（分析、权衡、列条件，中间不空行）。
THOUGHT = "先把这一段想清楚再动手，不然后面要返工。" * 40


def drain_now(master, sink):
    """把 PTY 里已经到的字节收走，不等。"""
    import select

    while select.select([master], [], [], 0)[0]:
        try:
            chunk = os.read(master, 65536)
        except OSError:
            return
        if not chunk:
            return
        sink.extend(chunk)


def job_logs():
    """沙箱里所有任务日志。"""
    root = h.HOME / "cache" / "jobs"
    if not root.exists():
        return []
    return sorted(root.rglob("*.log"))


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()

    chunk_sleep = 0.05
    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(h.STUB_PORT),
            STUB_REASONING="1",
            STUB_SUBAGENT="1",
            STUB_SUBBG="1",
            STUB_REASONING_TEXT=THOUGHT,
            STUB_CHUNK_SLEEP=str(chunk_sleep),
            STUB_SUBAGENT_BG_COMMAND="sleep 2; printf 'done\\n'",
            STUB_SUBAGENT_BG_ROUNDS="2",
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
        raise RuntimeError("桩模型没起来")
    daemon = subprocess.Popen(
        [str(h.BIN), "__daemon", "--port", str(h.PORT)],
        env=h.ENV,
        cwd=str(h.HOME),
        stdout=(OUT / "daemon.log").open("a"),
        stderr=subprocess.STDOUT,
    )
    if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
        raise RuntimeError("daemon 没起来")

    tui, master = h.spawn_tui()
    sink = bytearray()
    h.drain(master, 6.0, sink)
    # 等大厅画完再敲键：还在画的时候按下的键会丢。
    h.drain_until(master, sink, "A G E N T", 10.0)
    h.drain(master, 1.0, sink)
    # 先等输入框把字回显出来再回车：TUI 还在画大厅时按下的键会丢。
    message = "STUB_SUBBG 派一个后台子代理"
    os.write(master, message.encode())
    h.drain_until(master, sink, message, 5.0)
    started = time.time()
    os.write(master, b"\r")

    # 每 20ms 看一次日志：新出现的每一行记下时刻。
    #
    # **PTY 要一直读着**：不读的话 TUI 写屏写满缓冲就卡在 write 上，回车那一下
    # 还没来得及发给 daemon（离开大厅要先整屏重画）。能不能赶在卡住前发出去全
    # 凭时序——09-23 新旧两版二进制在 strace 下都 0 行落地。
    seen = 0
    events = []
    deadline = started + 60
    while time.time() < deadline:
        drain_now(master, sink)
        logs = job_logs()
        if logs:
            text = logs[-1].read_text(encoding="utf-8", errors="replace")
            lines = text.split("\n")
            while seen < len(lines) - 1:
                events.append(
                    {
                        "at_ms": round((time.time() - started) * 1000),
                        "tag": lines[seen][:8],
                        "chars": len(lines[seen]),
                    }
                )
                seen += 1
            if any(event["tag"].startswith("[正文]") for event in events):
                break
        time.sleep(0.02)

    (OUT / "screen.txt").write_text(
        "\n".join(h.render(bytes(sink))) + "\n", encoding="utf-8"
    )
    for process in (tui, daemon, stub):
        try:
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=5)
        except Exception:
            try:
                process.kill()
            except Exception:
                pass

    # `[思考+]` 也算：09-17 起同一段思考被时间闸切开时，后半截写成 `+` 标签
    # （读那侧据此粘回去，不另起一行）。量的是「面板多久能看到新字」，两种
    # 标签都是新字。
    thoughts = [
        event
        for event in events
        if event["tag"].startswith("[思考]") or event["tag"].startswith("[思考+]")
    ]
    gaps = [
        thoughts[i]["at_ms"] - thoughts[i - 1]["at_ms"] for i in range(1, len(thoughts))
    ]
    report = {
        "chunk_sleep_ms": round(chunk_sleep * 1000),
        "thought_chars_total": len(THOUGHT),
        "lines_landed": len(events),
        "thought_lines": len(thoughts),
        "new_paragraph_lines": sum(
            1 for event in thoughts if event["tag"].startswith("[思考]")
        ),
        "first_thought_at_ms": thoughts[0]["at_ms"] if thoughts else None,
        "gaps_between_thought_lines_ms": gaps,
        "max_gap_ms": max(gaps) if gaps else None,
        "chars_per_thought_line": [event["chars"] for event in thoughts],
        "events": events,
    }
    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(json.dumps({k: v for k, v in report.items() if k != "events"},
                     ensure_ascii=False, indent=2))
    print(f"\n产物：{OUT}")


if __name__ == "__main__":
    main()
