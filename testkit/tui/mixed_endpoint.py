#!/usr/bin/env python3
"""混合模型池时，全屏 TUI 每轮回复末尾要挂一行「本次供应商 / 模型」（BUG-05）：
前面空一行、暗色；重开 TUI 回放最近几轮时那一行也要在；设置成 off 就没有。

    cargo build
    python3 testkit/tui/mixed_endpoint.py

会话池两个模型（stub-model / stub-b），全局设置 mixed_model_endpoint_display 默认
interactive。桩模型会按请求里的 model 回，所以那一行应是 `stub / stub-model` 或
`stub / stub-b`。
"""

import json
import os
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

STUB = {"STUB_CHUNK_SLEEP": "0.01"}
FIRST = "第一句走查"
LINE = re.compile(r"stub / stub-(model|b)")


def mixed_config(display_value=None):
    providers = [{
        "id": "stub",
        "display_name": "Stub",
        "base_url": f"http://127.0.0.1:{h.STUB_PORT}/v1",
        "protocol": "openai-chat",
        "api_key": "stub",
        "models": ["stub-model", "stub-b"],
    }]
    extra = {
        "active_provider_models": [
            {"provider_id": "stub", "model": "stub-model"},
            {"provider_id": "stub", "model": "stub-b"},
        ],
        "providers": providers,
    }
    if display_value is not None:
        extra["display"] = {"mixed_model_endpoint_display": display_value}
    return extra


def send(master, sink, text, quiet=1.2, timeout=40):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def body_rows(screen):
    """正文行：去掉底部输入框与 footer。"""
    end = len(screen)
    for i in range(len(screen) - 1, -1, -1):
        if screen[i].startswith("┃") and "·" in screen[i]:
            end = i
            break
    while end > 0 and screen[end - 1].startswith("┃"):
        end -= 1
    return screen[:end]


def endpoint_row(rows):
    for i, line in enumerate(rows):
        if LINE.search(line):
            return i
    return None


def dim_at(row_index):
    """那一行的字是不是暗色（SGR 2）。pyte 没有 dim 位，直接查原始流里那一行前面
    最近的 SGR：这里只看最后一屏的字节里有没有 `\\x1b[2m` 紧跟着这行的文字。"""
    return None


def check_live(report):
    stub, daemon, tui, master, sink = r.start(STUB, mixed_config())
    try:
        screen = send(master, sink, FIRST)
        r.save("mixed-live", screen)
        rows = body_rows(screen)
        at = endpoint_row(rows)
        report["live_line_present"] = at is not None
        report["live_blank_line_before"] = at is not None and at > 0 and rows[at - 1].strip() == ""
        # 那一行在回复正文之后（回复正文里有「分块吐出来」）
        reply_at = next((i for i, l in enumerate(rows) if "分块吐出来" in l), None)
        report["live_line_after_reply"] = at is not None and reply_at is not None and at > reply_at
        # 暗色：原始字节流里那一行文字前面紧挨着 SGR 2
        raw = bytes(sink).decode("utf-8", "replace")
        m = LINE.search(raw)
        report["live_line_dim"] = bool(m) and "\x1b[2m" in raw[max(0, m.start() - 40):m.start()]

        # 重开 TUI：回放的那一轮也带这一行。
        #
        # 09-20 起 `miyu` 启动开的是新会话（05e57350），重开之后是空大厅，得用
        # `/session` 切回刚才那条才有回放（做法同 run.py item24）。换了 TUI 进程
        # 要 `reset_view()`：不清的话虚拟屏上还是第一个进程的残影，这一条以前一直
        # 红，看的就是那片残影。
        tui.send_signal(2)
        time.sleep(0.3)
        os.write(master, b"\x04")
        r.stop(tui)
        tui2, master2 = h.spawn_tui()
        sink2 = bytearray()
        h.reset_view()
        # 等大厅真的画出来再敲：新进程头一秒还没进 raw 模式（run.py item24）。
        deadline = time.time() + 20.0
        while time.time() < deadline and not any("Tab" in l for l in h.render(bytes(sink2))):
            h.settle(master2, sink2, quiet=0.2, timeout=1.0)
        os.write(master2, b"/session\r")
        # 「刚才那条」= 列表里第一条不叫「新会话」的（新开的那条还没命名）。
        for _ in range(12):
            deadline = time.time() + 10.0
            picked = ""
            while time.time() < deadline and not picked:
                h.drain(master2, 0.2, sink2)
                picked = next(
                    (l for l in h.render(bytes(sink2)) if "›" in l and " · " in l), ""
                )
            if picked and "新会话" not in picked and "New session" not in picked:
                break
            os.write(master2, b"j")
            h.settle(master2, sink2, quiet=0.25, timeout=1.0)
        os.write(master2, b"\r")
        h.drain_until(master2, sink2, FIRST, 30.0)
        h.settle(master2, sink2, quiet=1.0, timeout=20)
        screen2 = h.render(bytes(sink2))
        r.save("mixed-replay", screen2)
        rows2 = body_rows(screen2)
        at2 = endpoint_row(rows2)
        report["replay_line_present"] = at2 is not None and any(FIRST in l for l in rows2)
        r.stop(tui2)
    finally:
        r.stop(tui, daemon, stub)


def check_off(report):
    stub, daemon, tui, master, sink = r.start(STUB, mixed_config("off"))
    try:
        screen = send(master, sink, FIRST)
        r.save("mixed-off", screen)
        rows = body_rows(screen)
        report["off_hides_line"] = endpoint_row(rows) is None and any("分块吐出来" in l for l in rows)
    finally:
        r.stop(tui, daemon, stub)


def main():
    report = {}
    check_live(report)
    check_off(report)
    for key, ok in report.items():
        print(f"{'✅' if ok else '❌'} {key}")
    (h.OUT / "mixed-endpoint-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    passed = sum(1 for v in report.values() if v)
    print(f"\n{passed}/{len(report)} passed")
    sys.exit(0 if passed == len(report) else 1)


if __name__ == "__main__":
    main()
