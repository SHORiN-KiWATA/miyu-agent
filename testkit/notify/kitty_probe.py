#!/usr/bin/env python3
"""真机探针：kitty 的通知协议（OSC 99）在这台机器上到底成不成。

`testkit/tui/notify_sound.py` 验的是 Miyu 发出去的那串字节对不对；这个脚本验的
是**桌面那一侧**——同样一串字节写进一个 kitty 窗口之后：

1. 通知守护进程里真的多出一条（`makoctl list`，`app_name` = Miyu）；
2. 真的响了一声（PipeWire 多出一路 sink-input）；
3. 点它（`makoctl invoke`）真的把焦点跳回**发通知的那个 kitty 窗口**
   （`niri msg focused-window` 前后对比）——这一步会动你的焦点，所以要显式
   `--focus` 才跑。

跑法（tty 给一个跑在 kitty 里的终端设备，`ls -l /proc/<那个窗口里的进程>/fd/1`）：

    python3 testkit/notify/kitty_probe.py --tty /dev/pts/7
    python3 testkit/notify/kitty_probe.py --tty /dev/pts/7 --focus

依赖这台机器上的 kitty ≥ 0.36（`s=` 提示音）、一个支持 xdg-activation 的通知守
护进程（mako ≥ 1.10 行）、以及 freedesktop 声音主题（`sound-theme-freedesktop`）。
换了别的合成器/守护进程就只有第 3 条的查法要改。
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import time

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]


def b64(value):
    return base64.b64encode(value.encode()).decode()


def sequence(ident, title, body, sound, only="always"):
    return (
        f"\x1b]99;i={ident}:e=1:d=0:a=focus:o={only}:u=1"
        f":f={b64('Miyu')}:s={b64(sound)}:p=title;{b64(title)}\x1b\\"
        f"\x1b]99;i={ident}:e=1:d=1:p=body;{b64(body)}\x1b\\"
    )


def mako_list():
    out = subprocess.run(["makoctl", "list", "-j"], capture_output=True, text=True).stdout
    try:
        return json.loads(out)
    except Exception:
        return []


def niri(*args):
    socket = os.environ.get("NIRI_SOCKET")
    if not socket or not os.path.exists(socket):
        # 环境里那份可能是上一次会话留下的，按 pid 现找。
        candidates = sorted(
            f"/run/user/{os.getuid()}/{name}"
            for name in os.listdir(f"/run/user/{os.getuid()}")
            if name.startswith("niri.") and name.endswith(".sock")
        )
        if not candidates:
            return None
        socket = candidates[-1]
    env = dict(os.environ, NIRI_SOCKET=socket)
    out = subprocess.run(["niri", "msg", *args], capture_output=True, text=True, env=env)
    return out.stdout


def focused_window():
    out = niri("--json", "focused-window")
    try:
        return json.loads(out)
    except Exception:
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tty", required=True, help="跑在 kitty 里的终端设备")
    parser.add_argument("--sound", default="complete")
    parser.add_argument("--focus", action="store_true", help="连点击跳窗一起验（会动焦点）")
    parser.add_argument(
        "--file", help="自定义提示音那条路：kitty 闭嘴、这个文件由我们自己放"
    )
    args = parser.parse_args()

    report = {}
    subprocess.run(["makoctl", "dismiss", "-a"], capture_output=True)

    # 1 + 2：发一条，看通知栏里有没有、音频有没有多出一路。
    # 提示音就一秒出头，采一次多半采空——发完之后一直盯到它响完。
    before = set(sink_inputs())
    with open(args.tty, "w") as terminal:
        terminal.write(sequence("miyu-probe", "Miyu 回复完成", "正在等待处理", args.sound))
    appeared = set()
    deadline = time.time() + 2.5
    while time.time() < deadline:
        appeared |= set(sink_inputs()) - before
        time.sleep(0.1)
    shown = [n for n in mako_list() if n.get("app_name") == "Miyu"]
    report["通知守护进程收到了"] = bool(shown)
    report["标题正文没乱码"] = bool(shown) and (
        shown[0].get("summary") == "Miyu 回复完成"
        and shown[0].get("body") == "正在等待处理"
    )
    report["通知是可点的"] = bool(shown) and "default" in (shown[0].get("actions") or {})
    report["响了一声"] = bool(appeared)

    # 自定义文件那条路：Miyu 自己开播放器放，`notify.rs::play_tone` 的同一串
    # 命令按同样的顺序试一遍。
    if args.file:
        before = set(sink_inputs())
        played = None
        for player, argv in (
            ("canberra-gtk-play", ["-f", args.file]),
            ("pw-play", [args.file]),
            ("paplay", [args.file]),
        ):
            try:
                process = subprocess.Popen(
                    [player, *argv], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
                )
            except FileNotFoundError:
                continue
            appeared = set()
            deadline = time.time() + 2.5
            while time.time() < deadline:
                appeared |= set(sink_inputs()) - before
                time.sleep(0.1)
            process.wait()
            if appeared:
                played = player
                break
        report[f"自定义文件放得出来（{played or '一个播放器都不行'}）"] = bool(played)

    # 3：点它，焦点跳回发通知的那个窗口
    if args.focus:
        was = focused_window()
        target = None
        ids = [n["id"] for n in shown]
        if ids:
            subprocess.run(["makoctl", "invoke", "-n", str(ids[0])], capture_output=True)
            time.sleep(1.0)
            target = focused_window()
        report["点通知换了窗口"] = bool(
            was and target and was.get("id") != target.get("id")
        )
        report["跳到的是 kitty"] = bool(target and target.get("app_id") == "kitty")
        if was:
            niri("action", "focus-window", "--id", str(was["id"]))
    subprocess.run(["makoctl", "dismiss", "-a"], capture_output=True)

    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


def sink_inputs():
    """当前所有播放流的 id。响一声 = 这里多出来一个又消失。"""
    out = subprocess.run(
        ["pactl", "list", "short", "sink-inputs"], capture_output=True, text=True
    ).stdout
    return [line.split("\t")[0] for line in out.splitlines() if line.strip()]


if __name__ == "__main__":
    raise SystemExit(main())
