#!/usr/bin/env python3
"""TUI 预算实测：把施工单 §1 那张表量出来。

施工单写死了「任何一项守不住就回到 inline REPL，不硬上」，所以这个脚本量的是
**同一个二进制、同一个沙箱会话、同一个终端尺寸**下，inline REPL 与全屏 TUI 的
差值——不是绝对值。绝对值受会话内容影响太大，没有可比性。

    cargo build --release        # 预算要 release，debug 的体积与 RSS 不可比
    python3 testkit/tui/budget.py

产物在 ~/.cache/miyu-tui-budget/budget.json。
"""

import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import termios
import time
import urllib.error
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "release" / "miyu"
SMOKE = ROOT / "testkit" / "repl-smoke"

HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-tui-budget/home"))
RUNTIME = os.environ.get("MIYU_TUI_RUNTIME", "/tmp/mx-budget")
PORT = int(os.environ.get("MIYU_TUI_PORT", "18443"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18497"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-tui-budget"))
COLS, ROWS = 200, 60
IDLE_SECONDS = float(os.environ.get("IDLE_SECONDS", "5"))


def env_for(tui):
    env = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME)
    if tui:
        env["MIYU_TUI"] = "1"
    else:
        env.pop("MIYU_TUI", None)
    return env


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub",
            "display_name": "Stub",
            "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat",
            "api_key": "stub",
            "models": ["stub-model"],
        }],
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
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
            time.sleep(0.2)
    return False


def smaps(pid):
    """RSS / 匿名页，单位 KB。"""
    try:
        text = Path(f"/proc/{pid}/smaps_rollup").read_text()
    except OSError:
        return {}

    def field(name):
        for line in text.splitlines():
            if line.startswith(f"{name}:"):
                return int(line.split()[1])
        return 0

    return {"rss_kb": field("Rss"), "anon_kb": field("Anonymous")}


def cpu_jiffies(pid):
    try:
        parts = Path(f"/proc/{pid}/stat").read_text().split()
    except OSError:
        return 0
    # utime + stime
    return int(parts[13]) + int(parts[14])


def spawn(tui):
    master, slave = pty.openpty()
    import fcntl
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(BIN)], stdin=slave, stdout=slave, stderr=slave,
        env=env_for(tui), cwd=str(HOME), preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    return process, master


def drain(master, seconds):
    """读 seconds 秒，返回收到的字节数。"""
    total = 0
    deadline = time.time() + seconds
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if not ready:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        total += len(chunk)
    return total


def wait_quiet(master, quiet=0.12, timeout=3.0):
    """等到连续 `quiet` 秒没有输出为止。

    固定时长的 `drain` 对两种前端不公平：inline 空闲时一直在发 footer 的
    spinner 字节，固定窗口经常正好切在它的 tick 上，量出来的是「等下一个
    tick」而不是「按键到回显」。等静默才是同一把尺子。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], quiet)
        if not ready:
            return True
        try:
            if not os.read(master, 65536):
                return False
        except OSError:
            return False
    return False


def keystroke_latency(master, samples=15):
    """按键到第一个字节回来的时间，取中位数（毫秒）。"""
    times = []
    for _ in range(samples):
        wait_quiet(master)
        start = time.perf_counter()
        os.write(master, b"x")
        ready, _, _ = select.select([master], [], [], 1.0)
        if not ready:
            continue
        try:
            os.read(master, 65536)
        except OSError:
            break
        times.append((time.perf_counter() - start) * 1000)
        # 退格把刚打的字删掉，别让输入框越来越长影响后续采样
        os.write(master, b"\x7f")
    times.sort()
    return round(times[len(times) // 2], 2) if times else None


def measure(tui, label):
    """量一次。

    **每次都重建沙箱和 daemon**：共用一个 daemon 时，第二个跑的前端总是
    多出十几 MB（daemon 里攒下的状态让它加载更多东西），那个差值会被误读成
    「全屏比 inline 贵」——交换顺序一跑就露馅，差值跟着顺序走而不是跟着前端走。
    """
    if HOME.exists():
        shutil.rmtree(HOME)
    write_config()
    daemon = subprocess.Popen(
        [str(BIN), "__daemon", "--port", str(PORT)],
        env=env_for(False), cwd=str(HOME),
        stdout=(OUT / f"daemon-{label}.log").open("w"), stderr=subprocess.STDOUT,
    )
    if not wait_http(f"http://127.0.0.1:{PORT}/api/config", timeout=40):
        daemon.terminate()
        raise RuntimeError("daemon 没起来")
    process, master = spawn(tui)
    try:
        stages = {}
        # 分阶段采样，涨在哪一段一目了然
        time.sleep(0.6)
        stages["spawned"] = smaps(process.pid).get("rss_kb", 0)
        drain(master, 3.0)
        stages["first_paint"] = smaps(process.pid).get("rss_kb", 0)
        result = {"label": label, "stages": stages, **smaps(process.pid)}
        # 空闲：输出字节 + CPU
        before = cpu_jiffies(process.pid)
        idle_bytes = drain(master, IDLE_SECONDS)
        after = cpu_jiffies(process.pid)
        ticks = os.sysconf("SC_CLK_TCK")
        result["idle_output_bytes"] = idle_bytes
        result["idle_cpu_percent"] = round(
            (after - before) / ticks / IDLE_SECONDS * 100, 2
        )
        result["keystroke_ms"] = keystroke_latency(master)
        # 打完字之后再量一次，看输入有没有把内存顶上去
        stages["after_keys"] = smaps(process.pid).get("rss_kb", 0)
        result["rss_after_kb"] = stages["after_keys"]
        # 诊断用：内存分布明细
        try:
            text = Path(f"/proc/{process.pid}/smaps_rollup").read_text()
            (OUT / f"smaps-{label}.txt").write_text(text, encoding="utf-8")
            status = Path(f"/proc/{process.pid}/status").read_text()
            (OUT / f"status-{label}.txt").write_text(status, encoding="utf-8")
        except OSError:
            pass
        return result
    finally:
        for victim in (process, daemon):
            if victim.poll() is None:
                victim.terminate()
                try:
                    victim.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    victim.kill()
        os.close(master)
        # daemon 的 socket 要让出来，下一轮才能重新起
        time.sleep(1.0)


def main():
    if not BIN.exists():
        print(f"! 先 cargo build --release：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    write_config()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(STUB_PORT)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        # 顺序可配：验证「第二个跑的总是更大」这种顺序效应
        if os.environ.get("BUDGET_TUI_FIRST"):
            tui = measure(True, "tui")
            inline = measure(False, "inline")
        else:
            inline = measure(False, "inline")
            tui = measure(True, "tui")
    finally:
        for process in (stub,):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    report = {
        "binary_bytes": BIN.stat().st_size,
        "inline": inline,
        "tui": tui,
        "delta": {
            "rss_kb": tui["rss_kb"] - inline["rss_kb"],
            "anon_kb": tui["anon_kb"] - inline["anon_kb"],
        },
    }
    (OUT / "budget.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )

    print(f"{'项':<22}{'inline':>12}{'TUI':>12}{'差':>12}")
    print("-" * 58)
    print(f"{'空载 RSS (KB)':<20}{inline['rss_kb']:>12}{tui['rss_kb']:>12}"
          f"{tui['rss_kb'] - inline['rss_kb']:>+12}")
    print(f"{'匿名页 (KB)':<21}{inline['anon_kb']:>12}{tui['anon_kb']:>12}"
          f"{tui['anon_kb'] - inline['anon_kb']:>+12}")
    print(f"{'空闲输出 (字节)':<18}{inline['idle_output_bytes']:>12}"
          f"{tui['idle_output_bytes']:>12}"
          f"{tui['idle_output_bytes'] - inline['idle_output_bytes']:>+12}")
    print(f"{'空闲 CPU (%)':<21}{inline['idle_cpu_percent']:>12}"
          f"{tui['idle_cpu_percent']:>12}"
          f"{round(tui['idle_cpu_percent'] - inline['idle_cpu_percent'], 2):>+12}")
    print(f"{'按键到回显 (ms)':<18}{str(inline['keystroke_ms']):>12}"
          f"{str(tui['keystroke_ms']):>12}")
    print(f"\n二进制：{report['binary_bytes'] / 1024 / 1024:.2f} MB")
    print(f"产物：{OUT / 'budget.json'}")

    # 施工单 §1 的门槛
    budget = {
        "RSS 增量 ≤ 3 MB": report["delta"]["rss_kb"] <= 3 * 1024,
        "空闲输出 0 字节": tui["idle_output_bytes"] == 0,
        "空闲 CPU < 1%": tui["idle_cpu_percent"] < 1.0,
        "按键到回显 ≤ 16 ms": (tui["keystroke_ms"] or 999) <= 16,
    }
    print()
    for name, ok in budget.items():
        print(f"  {'✓' if ok else '✗'} {name}")
    return 0 if all(budget.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
