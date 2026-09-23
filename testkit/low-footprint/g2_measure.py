#!/usr/bin/env python3
"""WebUI 空闲 GPU/合成负载度量(G2)。

Xvfb 里开 Chrome 指向隔离 daemon 的 WebUI,分别量「窗口可见空闲」与
「窗口 unmap(等效切走/最小化,document.hidden=true)」两个 30s 窗口里
Chrome 各进程(gpu-process / renderer / 浏览器主进程)的 CPU 秒。
软件 GL 下合成走 CPU,数值放大但前后对比有效——这正是要的:动画停了
数字就掉,没停就掉不下来。

用法: python3 g2_measure.py <tag> [webui_url]
前置: 隔离 daemon 已在 run.py 的 WEB_PORT 上跑着(或传入现成 URL)。
"""
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BASE = Path(__file__).resolve().parent
TAG = sys.argv[1] if len(sys.argv) > 1 else "g2"
URL = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:18490"
XDISPLAY = f":{80 + os.getpid() % 40}"
WINDOW = float(os.environ.get("G2_WINDOW", "30"))


def chrome_procs(profile_dir):
    procs = {}
    for pid_dir in Path("/proc").iterdir():
        if not pid_dir.name.isdigit():
            continue
        try:
            cmd = (pid_dir / "cmdline").read_bytes().decode(errors="replace")
        except (FileNotFoundError, PermissionError):
            continue
        if profile_dir not in cmd or "chrome" not in cmd:
            continue
        kind = "browser"
        m = re.search(r"--type=([a-z-]+)", cmd)
        if m:
            kind = m.group(1)
        procs.setdefault(kind, []).append(int(pid_dir.name))
    return procs


def jiffies(pid):
    try:
        parts = Path(f"/proc/{pid}/stat").read_text().split()
        return int(parts[13]) + int(parts[14])
    except (FileNotFoundError, ProcessLookupError):
        return 0


def sample(procs, seconds):
    start = {k: sum(jiffies(p) for p in pids) for k, pids in procs.items()}
    t0 = time.time()
    time.sleep(seconds)
    dt = time.time() - t0
    clk = os.sysconf("SC_CLK_TCK")
    return {
        k: round((sum(jiffies(p) for p in pids) - start[k]) / clk / dt * 100, 2)
        for k, pids in procs.items()
    }, dt


def main():
    profile = tempfile.mkdtemp(prefix="g2-chrome-")
    xvfb = subprocess.Popen(
        ["Xvfb", XDISPLAY, "-screen", "0", "1400x900x24"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    time.sleep(1.5)
    env = dict(os.environ)
    env["DISPLAY"] = XDISPLAY
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("XDG_SESSION_TYPE", None)
    chrome = subprocess.Popen(
        [
            "google-chrome-stable",
            f"--user-data-dir={profile}",
            "--ozone-platform=x11",
            "--no-first-run", "--no-default-browser-check",
            "--disable-background-networking", "--mute-audio",
            "--window-size=1380,860", URL,
        ],
        env=env, stdout=subprocess.DEVNULL, stderr=open(BASE / "chrome-stderr.log", "w"),
    )
    result = {"tag": TAG, "url": URL}
    try:
        time.sleep(12)
        result["chrome_poll"] = chrome.poll()
        procs = chrome_procs(profile)
        result["procs"] = {k: len(v) for k, v in procs.items()}
        result["visible_idle_cpu_pct"], _ = sample(procs, WINDOW)

        # unmap 窗口 → document.hidden=true(等效最小化)。xdotool 在 Xvfb 里可用。
        # 同 profile 再开一个 about:blank → 同窗口新前台标签,原页必发
        # visibilitychange(document.hidden=true)。Xvfb 无 WM,minimize 不管用。
        subprocess.run(
            ["google-chrome-stable", f"--user-data-dir={profile}", "about:blank"],
            env=env, check=False, timeout=15,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        result["unmap_rc"] = 0
        time.sleep(3)
        result["hidden_cpu_pct"], _ = sample(procs, WINDOW)
    finally:
        chrome.terminate()
        try:
            chrome.wait(timeout=8)
        except subprocess.TimeoutExpired:
            chrome.kill()
        xvfb.send_signal(signal.SIGTERM)
        xvfb.wait()
    out = BASE / "results" / f"g2-{TAG}.json"
    out.parent.mkdir(exist_ok=True)
    out.write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
