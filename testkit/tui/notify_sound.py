#!/usr/bin/env python3
"""通知音效 + 点通知跳回窗口（BUG-11 / BUG-12）走查。

在 kitty 里通知改由**终端自己**弹（通知协议 OSC 99）：只有它能在点击时把自己的
窗口拉到前面（Wayland 下外部进程抢不了焦点），顺带按系统声音主题响一声。这里验
的是 Miyu 这一侧——**发没发、发的是什么**：真 PTY 里跑一轮带提问的对话，把终端
收到的原始字节捞出来，按 kitty 的协议拆开逐项对。

kitty/mako/niri 那一侧（通知真的弹出来了、真的响了、点了真的跳回窗口）不在这个
脚本里，它需要活的桌面会话，走 `testkit/notify/kitty_probe.py`。

跑法：

    cargo build
    python3 testkit/tui/notify_sound.py

产物在 ~/.cache/miyu-notify-sound/。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import base64
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-notify-sound"))
PROMPT = "走查一句"
# 一段 OSC 99：`ESC ] 99 ; <元数据> ; <载荷> ESC \`
OSC99 = re.compile(rb"\x1b\]99;([^;]*);([^\x1b]*)\x1b\\")


def notifications(raw):
    """把字节流里的 OSC 99 拆成 [(字段表, 载荷)]，base64 的字段顺手解开。"""
    found = []
    for meta, payload in OSC99.findall(raw):
        fields = {}
        for part in meta.decode().split(":"):
            if "=" in part:
                key, value = part.split("=", 1)
                fields[key] = value
        for key in ("f", "s"):
            if key in fields:
                fields[key] = base64.b64decode(fields[key]).decode()
        try:
            text = base64.b64decode(payload).decode() if fields.get("e") == "1" else payload.decode()
        except Exception:
            text = "<解不开>"
        found.append((fields, text))
    return found


def titles(raw):
    """按 `i=` 把标题段和正文段拼回一条条通知：[(标题, 正文, 标题段字段)]。"""
    chunks = notifications(raw)
    out = []
    for fields, text in chunks:
        if fields.get("p", "title") == "title":
            out.append([text, "", fields])
        elif out:
            out[-1][1] = text
    return out


def write_config(extra):
    (h.HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub",
            "display_name": "Stub",
            "base_url": f"http://127.0.0.1:{h.STUB_PORT}/v1",
            "protocol": "openai-chat",
            "api_key": "stub",
            "models": ["stub-model"],
        }],
        "memory": {"enabled": False},
    }
    config.update(extra)
    (h.HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def run_one_turn(term):
    """起一个 TUI、跑一轮（提问 → 回答 → 回复完成）、返回终端收到的原始字节。"""
    h.ENV = dict(h.ENV, TERM=term)
    tui, master = h.spawn_tui()
    sink = bytearray()
    try:
        h.drain(master, 3.0, sink)
        os.write(master, PROMPT.encode())
        h.drain_until(master, sink, PROMPT, 3.0)
        os.write(master, b"\r")
        # 提问面板：回车选第一项
        h.drain_until(master, sink, "走查用的问题", 30.0)
        h.drain(master, 0.5, sink)
        os.write(master, b"\r")
        h.drain_until(master, sink, "走查的回复", 30.0)
        h.drain(master, 1.5, sink)
    finally:
        try:
            os.write(master, b"\x03")
            time.sleep(0.3)
        except OSError:
            pass
        tui.terminate()
        try:
            tui.wait(timeout=5)
        except subprocess.TimeoutExpired:
            tui.kill()
        os.close(master)
    return bytes(sink)


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    write_config({})
    h.kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), STUB_ASK="1"),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        # ── 1. kitty + 默认配置：两条通知各带各的音 ──────────────────────
        raw = run_one_turn("xterm-kitty")
        (OUT / "kitty-default.bin").write_bytes(raw)
        sent = titles(raw)
        (OUT / "kitty-default.json").write_text(
            json.dumps(sent, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        by_title = {title: (body, fields) for title, body, fields in sent}
        report["提问发通知"] = "Miyu 在等你回答" in by_title
        report["回复完成发通知"] = "Miyu 回复完成" in by_title
        question = by_title.get("Miyu 在等你回答", ("", {}))
        done = by_title.get("Miyu 回复完成", ("", {}))
        report["正文是「正在等待处理」"] = (
            question[0] == "正在等待处理" and done[0] == "正在等待处理"
        )
        # 默认用 Miyu 内置的木琴音（用户 09-18 挑的：两个事件都用 wake）。
        # kitty 的 `s=` 只认声音主题里的名字、喂不了文件路径，所以让它闭嘴，
        # 音由 Miyu 自己放——通知栏里看不到，看的是它有没有落到盘上。
        report["默认让 kitty 闭嘴(音自己放)"] = (
            question[1].get("s") == "silent" and done[1].get("s") == "silent"
        )
        sounds = h.HOME / "cache" / "sounds"
        assets = Path(__file__).resolve().parents[2] / "assets" / "voice"
        report["内置音落到缓存目录"] = (sounds / "wake.wav").exists()
        report["落的就是 wake 那一段"] = (sounds / "wake.wav").read_bytes() == (
            assets / "wake.wav"
        ).read_bytes()
        report["两条都带 focus 动作"] = all(
            fields.get("a") == "focus" for _, _, fields in sent
        )
        report["两条都交给 kitty 判焦点"] = all(
            fields.get("o") == "unfocused" for _, _, fields in sent
        )
        report["同一进程复用一个 id"] = (
            len({fields.get("i") for _, _, fields in sent}) == 1
            and re.fullmatch(r"miyu-\d+", sent[0][2].get("i", "")) is not None
        )
        report["标题正文都走 base64"] = all(
            fields.get("e") == "1" for _, _, fields in sent
        )

        # ── 2. 关掉提示音：通知照发，音改成 silent ──────────────────────
        write_config({"notifications": {"sound": False}})
        raw = run_one_turn("xterm-kitty")
        (OUT / "kitty-muted.bin").write_bytes(raw)
        muted = titles(raw)
        report["关音后通知还在"] = any(
            title == "Miyu 回复完成" for title, _, _ in muted
        )
        report["关音后音名是 silent"] = bool(muted) and all(
            fields.get("s") == "silent" for _, _, fields in muted
        )

        # ── 3. 换成自己的音频文件：通知照发，但让 kitty 闭嘴（`s=` 只认声音
        #      主题里的名字，文件得我们自己放）────────────────────────────
        custom = Path("/tmp/miyu-notify-sound/ding.wav")
        custom.parent.mkdir(parents=True, exist_ok=True)
        custom.write_bytes(b"RIFF\x00\x00\x00\x00WAVE")
        write_config({"notifications": {"sound_file": str(custom)}})
        raw = run_one_turn("xterm-kitty")
        (OUT / "kitty-custom.bin").write_bytes(raw)
        custom_sent = titles(raw)
        report["自定义文件时通知还在"] = any(
            title == "Miyu 回复完成" for title, _, _ in custom_sent
        )
        report["自定义文件时 kitty 不出声"] = bool(custom_sent) and all(
            fields.get("s") == "silent" for _, _, fields in custom_sent
        )

        # ── 4. 自定义文件指到不存在的路径：退回内置音，别变哑的。退到主题音
        #      的话 `s=` 会写成 `complete`，所以这里仍是 silent 才对 ────────
        write_config({"notifications": {"sound_file": "/nonexistent/miyu-ding.wav"}})
        raw = run_one_turn("xterm-kitty")
        (OUT / "kitty-missing-file.bin").write_bytes(raw)
        missing = {title: fields for title, _, fields in titles(raw)}
        report["文件不存在仍发通知"] = "Miyu 回复完成" in missing
        report["文件不存在退回内置音"] = (
            missing.get("Miyu 回复完成", {}).get("s") == "silent"
        )

        # ── 5. 关掉桌面通知：一条都不发 ────────────────────────────────
        write_config({"notifications": {"enabled": False}})
        raw = run_one_turn("xterm-kitty")
        (OUT / "kitty-off.bin").write_bytes(raw)
        report["关通知后一条都不发"] = not titles(raw)

        # ── 6. 非 kitty 终端：不往终端里吐这串转义（会变成一行乱码）────
        write_config({})
        raw = run_one_turn("xterm-256color")
        (OUT / "plain-term.bin").write_bytes(raw)
        report["非 kitty 不发 OSC 99"] = not titles(raw)
    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
