#!/usr/bin/env python3
"""daemon 忘掉拉起它的 herdr pane(09-23):它起的子进程拿不到 pane 坐标。

    BIN=<miyu 二进制> python3 testkit/herdr/daemon_env.py

daemon 由哪个 pane 里的客户端拉起,就继承了哪个 pane 的 `HERDR_PANE_ID` 等坐标;
它再起的子进程(中转线 CLI、run_command 跑的 claude……)装着 herdr 钩子的话,会拿着
坐标去认领那个 pane,herdr 从此丢掉 Miyu 的上报(真 herdr 复现见 `real_herdr.py`)。

中转线那边另有一层兜底(起 CLI 时把 HERDR_* 全剥),所以从假 agy 身上分不出 daemon
自己有没有忘。这里换一条不经中转线的路:桩模型让 daemon 跑一次 run_command,命令
把自己看到的 HERDR_* 写进文件。`/proc/<daemon>/environ` 看不出来——那是进程启动那
一刻的环境区,进程里删掉变量它也不变。

判定:
  child_ran             daemon 真的替我们跑了那条命令
  coordinates_forgotten 子进程看不到 HERDR_PANE_ID / HERDR_TAB_ID / HERDR_WORKSPACE_ID
  herdr_stamp_kept      HERDR_ENV / HERDR_SOCKET_PATH 还在(daemon 画 kitty 图的判据靠它们,这次不动)
一条判定一行 ✅/❌,最后 n/m passed。
"""
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu").resolve()
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-herdr-daemon-env")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
CHILD_ENV = OUT / "child-env.txt"
PORT = int(os.environ.get("PORT", "18563"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18564"))
STUB = REPO / "testkit" / "repl-smoke" / "stub_llm.py"

# 跑测具的进程自己的 HERDR_* 一个都不能漏进来,下面只给 daemon 一组假的。
BASE_ENV = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
BASE_ENV.update(MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))
FAKE_PANE = {
    "HERDR_ENV": "1",
    "HERDR_PANE_ID": "w9:p9",
    "HERDR_TAB_ID": "w9:t9",
    "HERDR_WORKSPACE_ID": "w9",
    "HERDR_SOCKET_PATH": str(OUT / "no-such-herdr.sock"),
    "HERDR_BIN_PATH": "/bin/false",
}

results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{('  ' + str(detail)[:300]) if detail else ''}", flush=True)


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.3)
    return False


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"],
        }],
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2), "utf-8")
    (HOME / "state").mkdir(parents=True, exist_ok=True)
    (HOME / "state" / "daemon-launch.json").write_text(json.dumps({"port": PORT}), "utf-8")


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True)
    write_config()
    command = f"env | grep '^HERDR_' > {CHILD_ENV}; true"
    stub = subprocess.Popen(
        [sys.executable, str(STUB)],
        env=dict(BASE_ENV, STUB_PORT=str(STUB_PORT), STUB_TOOL="1", STUB_TOOL_COMMAND=command,
                 STUB_REPLY="好了"),
        stdout=(OUT / "stub.log").open("w"), stderr=subprocess.STDOUT,
    )
    daemon = subprocess.Popen(
        [str(BIN), "__daemon", "--port", str(PORT)], env=dict(BASE_ENV, **FAKE_PANE), cwd=str(HOME),
        stdin=subprocess.DEVNULL, stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
    )
    try:
        assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon 没起来(看 daemon.log)"
        # 客户端不带任何 HERDR_*:这里只看 daemon 那一侧。
        ask = subprocess.run(
            [str(BIN), "ask", "--output-format", "json", "--session", "herdr-env", "--create", "走查一句"],
            env=BASE_ENV, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120,
        )
        (OUT / "ask.out").write_text(ask.stdout + "\n---\n" + ask.stderr, "utf-8")
        seen = CHILD_ENV.read_text("utf-8").splitlines() if CHILD_ENV.exists() else None
        check("child_ran", seen is not None, f"ask 退出码 {ask.returncode}: {ask.stderr[-200:]}")
        names = {line.split("=", 1)[0] for line in (seen or [])}
        leaked = sorted(names & {"HERDR_PANE_ID", "HERDR_TAB_ID", "HERDR_WORKSPACE_ID"})
        check("coordinates_forgotten", seen is not None and not leaked, f"子进程看到的: {sorted(names)}")
        check("herdr_stamp_kept", {"HERDR_ENV", "HERDR_SOCKET_PATH"} <= names, f"子进程看到的: {sorted(names)}")
    finally:
        daemon.terminate()
        stub.terminate()
        for process in (daemon, stub):
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({OUT})")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
