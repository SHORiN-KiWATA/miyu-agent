#!/usr/bin/env python3
"""agy 进程复用真机 A/B(09-18):真 agy + 沙箱 daemon,量每轮 wall 时间。

    BIN=<miyu> python3 testkit/agy-reuse/live.py

会花真实 agy 调用(gemini flash,每轮一句「只回一个字」),不进 CI。桥关着
(miyu_tools/native_tools=off),不碰真实 ~/.gemini/config/mcp_config.json;人格代理文件
按内容哈希落 ~/.gemini/config/agents/miyu-<hash>(生产同款,1 小时后自动回收)。

三组:A 复用开(4 轮) → B 复用关(4 轮) → C 复用开(3 轮,抵消顺序效应)。
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu")
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-agy-live")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18553"))
MODEL = os.environ.get("AGY_MODEL", "gemini-3.8-flash-high")
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")


def write_config(reuse):
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "antigravity",
        "active_provider_models": [{"provider_id": "antigravity", "model": MODEL}],
        "providers": [{
            "id": "antigravity", "display_name": "Antigravity", "base_url": "", "protocol": "antigravity",
            "api_key": "", "models": [MODEL], "enabled": True,
        }],
        "memory": {"enabled": False},
        "plugins": {"antigravity": {
            "native_tools": "off", "miyu_tools": "off",
            "reuse_process": reuse, "reuse_idle_seconds": 600,
        }},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2), "utf-8")


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def cli(args, timeout=180):
    proc = subprocess.run([str(BIN), *args], env=ENV, stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=timeout)
    return proc.returncode, proc.stdout, proc.stderr


def ask(session, text, create=False):
    args = ["ask", "--output-format", "json", "--session", session]
    if create:
        args.append("--create")
    started = time.time()
    code, out, err = cli([*args, text])
    elapsed = time.time() - started
    reply = ""
    for line in out.splitlines():
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if obj.get("type") == "done" or "text" in obj:
            reply = obj.get("text", "") or reply
    return code, round(elapsed, 2), reply.strip()[:40], err.strip()[-160:]


def relay_lines():
    lines = []
    for log in sorted((HOME / "cache" / "logs").glob("miyu.*.log")):
        for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
            if "miyu::relay" in line:
                lines.append(line)
    return lines


def run_arm(name, session, turns, reuse):
    rows = []
    for index in range(turns):
        code, elapsed, reply, err = ask(session, f"TK 第{index + 1}句,只回一个字", create=(index == 0))
        rows.append((index + 1, elapsed, code, reply, err))
        print(f"  {name} turn {index + 1}: {elapsed:.2f}s code={code} reply={reply!r} {err[:60]}", flush=True)
    return rows


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config(reuse=True)
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                              stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    arms = {}
    try:
        assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon not up"
        time.sleep(1.0)
        arms["A 复用开"] = run_arm("A", "a", 4, True)
        write_config(reuse=False)
        assert cli(["reload"])[0] == 0
        arms["B 复用关"] = run_arm("B", "b", 4, False)
        write_config(reuse=True)
        assert cli(["reload"])[0] == 0
        arms["C 复用开"] = run_arm("C", "c", 3, True)
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:
            daemon.kill()
        lines = relay_lines()
        reused = sum("reusing pooled agy process" in l for l in lines)
        (OUT / "relay.log").write_text("\n".join(lines), "utf-8")
        (OUT / "arms.json").write_text(json.dumps(arms, ensure_ascii=False, indent=2), "utf-8")
    print("\n| 组 | 轮 | 用时 s | 回复 |")
    print("|---|---|---|---|")
    for name, rows in arms.items():
        for turn, elapsed, code, reply, _ in rows:
            print(f"| {name} | {turn} | {elapsed:.2f} | {reply if code == 0 else 'ERR'} |")
    print(f"\nreusing lines in daemon log: {reused}")
    for l in lines:
        m = re.search(r"(\S+Z).*?(reusing|retiring|parking)(.*)", l)
        if m:
            print("  ", m.group(1)[11:23], m.group(2), m.group(3)[:110])


if __name__ == "__main__":
    main()
