#!/usr/bin/env python3
"""真模型看一眼：子代理只在后台跑之后，模型派完是不是就收这一轮、不空转查状态（09-26）。

沙箱 MIYU_HOME + 真配置的一份拷贝（不动 ~/.miyu；平台、网页、语音、闹钟都摘掉）+ 独立端口 daemon。
一次性 `miyu ask --output-format stream-json` 让模型派一个子代理，按事件流逐轮看工具调用：

    dispatched          第一轮调了 subagent
    no_polling          第一轮派完没有去查任务状态（job 工具）、没有 sleep 干等
    main_turn_ended     第一轮派完就收了，后面另起了一轮（报告叫醒的）
    concluded           最后那条 done 带着结论（和第一轮说的不一样）

要真模型、会花一点钱，不进红绿账：

    python3 testkit/subagent-session/live_model.py [--model 供应商/模型]
"""
import argparse
import importlib.util
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "testkit"))
import sandbox_dir  # noqa: E402

spec = importlib.util.spec_from_file_location("persona_ab", REPO / "testkit" / "persona-ab" / "run.py")
persona_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(persona_ab)

BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu")
SANDBOX = sandbox_dir.make("miyu-subagent-live-")
HOME = SANDBOX / "home"
RUNTIME = SANDBOX / "runtime"

PROMPT = (
    "请用 subagent 工具派一个子代理去统计 /etc/passwd 有多少行（让它自己跑命令），"
    "等它的结论回来再告诉我行数。你自己不要去查。"
)


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def env():
    e = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), LANG="zh_CN.UTF-8")
    for key in ("MIYU_DIRECT", "MIYU_SESSION", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
                "XDG_STATE_HOME"):
        e.pop(key, None)
    return e


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", help="供应商/模型，缺省用配置里的活跃模型")
    args = parser.parse_args()
    assert BIN.exists(), f"missing binary {BIN}"
    (HOME / "config").mkdir(parents=True)
    RUNTIME.mkdir(parents=True)
    cfg = persona_ab.load_real_config()
    for key in ("platforms", "web", "voice", "alarm"):
        cfg.pop(key, None)
    cfg.setdefault("memory", {})["enabled"] = False
    (HOME / "config" / "config.jsonc").write_text(json.dumps(cfg, ensure_ascii=False, indent=2), "utf-8")
    port = free_port()
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(port)], env=env(), cwd=str(HOME),
                              stdin=subprocess.DEVNULL, stdout=(SANDBOX / "daemon.log").open("w"),
                              stderr=subprocess.STDOUT)
    try:
        deadline = time.time() + 40
        while time.time() < deadline:
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=2)
                break
            except Exception:
                time.sleep(0.3)
        argv = [str(BIN), "ask", "--output-format", "stream-json"]
        if args.model:
            argv += ["--model", args.model]
        started = time.time()
        proc = subprocess.run([*argv, PROMPT], env=env(), stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=900)
        seconds = round(time.time() - started, 1)
    finally:
        daemon.terminate()
        try:
            daemon.wait(15)
        except subprocess.TimeoutExpired:
            daemon.kill()

    events = [json.loads(line) for line in proc.stdout.splitlines() if line.strip().startswith("{")]
    runs, current = [], None
    for event in events:
        if event.get("type") == "started":
            current = {"tools": [], "text": ""}
            runs.append(current)
        elif current is not None and event.get("type") == "tool" and event.get("phase") == "start":
            current["tools"].append((event.get("name"), json.dumps(event.get("arguments"), ensure_ascii=False)[:80]))
        elif current is not None and event.get("type") == "text":
            current["text"] += event.get("delta", "")
    done = events[-1] if events and events[-1].get("type") == "done" else {}
    for index, run in enumerate(runs):
        print(f"--- 第 {index + 1} 轮 ---")
        for name, arguments in run["tools"]:
            print(f"  工具 {name} {arguments}")
        print(f"  正文 {run['text'][:160]!r}")
    print(f"退出码 {proc.returncode}，用时 {seconds}s，done：{done.get('text', '')[:200]!r}")

    first = runs[0] if runs else {"tools": [], "text": ""}
    names = [name for name, _ in first["tools"]]
    after_dispatch = names[names.index("subagent") + 1:] if "subagent" in names else []
    results = {
        "dispatched": "subagent" in names,
        "no_polling": not any(name in ("job", "job_status") or "sleep" in arguments
                              for name, arguments in first["tools"][len(names) - len(after_dispatch):]),
        "main_turn_ended": len(runs) >= 2,
        "concluded": bool(done.get("text")) and done.get("text", "").strip() != first["text"].strip(),
    }
    for name, ok in results.items():
        print(f"{'✅' if ok else '❌'} {name}")
    passed = sum(results.values())
    print(f"{passed}/{len(results)} passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
