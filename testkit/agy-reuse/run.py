#!/usr/bin/env python3
"""agy 进程复用黑盒(09-18):沙箱 daemon + 假 agy(stream-json,一个进程连打多轮)。

    BIN=<miyu> python3 testkit/agy-reuse/run.py

假 agy 每轮把 pid / 轮次 / stdin 长度记进日志;daemon 侧配 antigravity 供应商指向它。

判定:
  same_process        同一会话连发三轮,三轮同一个 pid,轮次 1/2/3
  delta_only          第二轮起 stdin 只发增量(第一轮那句不再出现)
  idle_reaped         闲置超过 reuse_idle_seconds 后下一轮换新进程、旧进程收掉,新进程带 --conversation 续传
  reload_retires      miyu reload 后池里的进程收掉
  delete_retires      删除会话后它名下的进程收掉
  reuse_off           关掉复用后每轮一个新进程,且轮结束就退出
  no_orphans          daemon 停掉后没有假 agy 残留
"""
import json
import os
import shutil
import stat
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
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-agy-reuse")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
AGY_CONFIG = OUT / "agy-config"
FAKE = OUT / "fake-agy"
LOG = OUT / "fake-agy.jsonl"
PORT = int(os.environ.get("PORT", "18543"))
# daemon 日志开到 info:池的借/还/收(miyu::relay)都在 info,出问题能看出为什么。
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME),
           MIYU_AGY_CONFIG_DIR=str(AGY_CONFIG), FAKE_AGY_LOG=str(LOG),
           MIYU_LOG=os.environ.get("MIYU_LOG", "info"))

FAKE_AGY = r'''#!/usr/bin/env python3
import json, os, sys, time
args = sys.argv[1:]
def opt(name):
    return args[args.index(name) + 1] if name in args else None
agent = opt("--agent") or ""
resumed = opt("--conversation")
sid = resumed or f"fake-{os.getpid()}-{int(time.time() * 1000) % 100000}"
log = os.environ.get("FAKE_AGY_LOG")
def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n"); sys.stdout.flush()
def record(obj):
    if log:
        with open(log, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")
usage = {"input_tokens": 10, "output_tokens": 2, "thinking_tokens": 0, "cache_read_tokens": 0, "total_tokens": 12}
emit({"event": "init", "conversation_id": sid, "init": {"model": "m", "cwd": "/", "agent": agent, "tools": []}})
record({"kind": "start", "pid": os.getpid(), "sid": sid, "resumed": resumed is not None})
turn = 0
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    turn += 1
    body = f"reply {turn} from pid {os.getpid()}"
    emit({"event": "step_update", "step_update": {"conversation_id": sid, "step_index": 2 * turn - 1, "state": "DONE", "step_type": "user_input"}})
    emit({"event": "step_update", "step_update": {"conversation_id": sid, "step_index": 2 * turn, "state": "DONE", "step_type": "agent_response", "text_delta": body, "usage": usage}})
    emit({"event": "result", "result": {"conversation_id": sid, "status": "SUCCESS", "response": body, "num_turns": turn, "usage": usage}})
    record({"kind": "turn", "pid": os.getpid(), "sid": sid, "turn": turn, "stdin_chars": len(line), "stdin": line[:4000]})
record({"kind": "exit", "pid": os.getpid(), "sid": sid, "turns": turn})
'''


def write_config(reuse=True, idle=3):
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "agy",
        "active_provider_models": [{"provider_id": "agy", "model": "m"}],
        "providers": [{
            "id": "agy", "display_name": "Agy", "base_url": "", "protocol": "antigravity",
            "api_key": "", "models": ["m"], "enabled": True,
        }],
        "memory": {"enabled": False},
        "plugins": {"antigravity": {
            "binary": str(FAKE), "native_tools": "off", "miyu_tools": "off",
            "reuse_process": reuse, "reuse_idle_seconds": idle,
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


def cli(args, timeout=120):
    # stdin 必须显式给 /dev/null:CLI 会探测管道 stdin 最多等 5 秒,继承一个没人关的
    # 管道时每次 ask 白等 5 秒,正好撞上测试用的闲置回收阈值,复用就「莫名」失效。
    proc = subprocess.run([str(BIN), *args], env=ENV, stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=timeout)
    return proc.returncode, proc.stdout, proc.stderr


ASK_SECONDS = []


def ask(session, text, create=False):
    args = ["ask", "--output-format", "json", "--session", session]
    if create:
        args.append("--create")
    started = time.time()
    code, out, err = cli([*args, text])
    ASK_SECONDS.append(round(time.time() - started, 2))
    assert code == 0, f"ask failed: {err[-400:]}"
    return out


def records():
    if not LOG.exists():
        return []
    return [json.loads(line) for line in LOG.read_text(encoding="utf-8").splitlines() if line.strip()]


def turns_since(index):
    return [r for r in records()[index:] if r["kind"] == "turn"]


def alive(pid):
    try:
        state = Path(f"/proc/{pid}/stat").read_text().split(")")[-1].split()[0]
    except FileNotFoundError:
        return False
    return state != "Z"


def gone_soon(pid, wait=8.0):
    deadline = time.time() + wait
    while time.time() < deadline:
        if not alive(pid):
            return True
        time.sleep(0.2)
    return False


results = {}


def check(name, ok, detail=""):
    results[name] = bool(ok)
    print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:220]}", flush=True)


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    if OUT.exists():
        shutil.rmtree(OUT)
    for path in (RUNTIME, AGY_CONFIG):
        path.mkdir(parents=True, exist_ok=True)
    FAKE.write_text(FAKE_AGY, "utf-8")
    FAKE.chmod(FAKE.stat().st_mode | stat.S_IEXEC)
    # 运行时把闲置回收下限钉在 5 秒(别把进程回收得比一句话的间隔还快);这里给 6,
    # 三连问之间的间隔(每次 ask 约 0.2 秒)远小于它。
    write_config(reuse=True, idle=6)
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                              stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    try:
        assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon not up"
        time.sleep(1.0)

        # 1. 同会话三轮 → 一个进程
        ask("s", "TK one", create=True)
        ask("s", "TK two")
        ask("s", "TK three")
        turns = turns_since(0)
        pids = {t["pid"] for t in turns}
        check("same_process", len(turns) == 3 and len(pids) == 1 and [t["turn"] for t in turns] == [1, 2, 3],
              {"pids": sorted(pids), "turns": [t["turn"] for t in turns]})
        check("delta_only", len(turns) >= 2 and "TK one" in turns[0]["stdin"] and "TK one" not in turns[1]["stdin"]
              and "TK two" in turns[1]["stdin"],
              {"first": turns[0]["stdin_chars"], "second": turns[1]["stdin_chars"]} if len(turns) >= 2 else turns)
        pid_a = turns[0]["pid"] if turns else None

        # 2. 闲置回收:超过 reuse_idle_seconds 后换新进程,带 --conversation 续传
        mark = len(records())
        time.sleep(8.0)
        ask("s", "TK four")
        new = turns_since(mark)
        starts = [r for r in records()[mark:] if r["kind"] == "start"]
        check("idle_reaped", len(new) == 1 and new[0]["pid"] != pid_a and starts and starts[-1]["resumed"]
              and gone_soon(pid_a),
              {"old": pid_a, "new": [t["pid"] for t in new], "resumed": [s["resumed"] for s in starts]})
        pid_b = new[0]["pid"] if new else None

        # 3. reload 收掉池里的进程
        code, _, err = cli(["reload"])
        check("reload_retires", code == 0 and pid_b is not None and gone_soon(pid_b), {"code": code, "pid": pid_b, "err": err.strip()[:80]})

        # 4. 删除会话收掉它名下的进程
        mark = len(records())
        ask("s2", "TK five", create=True)
        pid_c = turns_since(mark)[0]["pid"]
        code, _, err = cli(["session", "delete", "--yes", "s2"])
        check("delete_retires", code == 0 and gone_soon(pid_c), {"code": code, "pid": pid_c, "err": err.strip()[:80]})

        # 5. 关掉复用:每轮一个新进程,轮结束就退出
        write_config(reuse=False, idle=6)
        code, _, _ = cli(["reload"])
        mark = len(records())
        ask("s3", "TK six", create=True)
        ask("s3", "TK seven")
        new = turns_since(mark)
        exits = [r for r in records()[mark:] if r["kind"] == "exit"]
        check("reuse_off", len(new) == 2 and new[0]["pid"] != new[1]["pid"] and len(exits) == 2
              and all(gone_soon(t["pid"], 5) for t in new),
              {"pids": [t["pid"] for t in new], "exits": len(exits)})
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=15)
        except subprocess.TimeoutExpired:
            daemon.kill()
        time.sleep(1.0)
        leftovers = [r["pid"] for r in records() if r["kind"] == "start" and alive(r["pid"])]
        check("no_orphans", not leftovers, leftovers)
        (OUT / "verdict.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), "utf-8")
        print("ask seconds:", ASK_SECONDS)
        if not all(results.values()):
            print("--- relay log lines")
            for log in sorted((HOME / "cache" / "logs").glob("miyu.*.log")):
                for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
                    if "miyu::relay" in line or "agy" in line:
                        print("  ", line[:240])
            print("--- fake agy records")
            for r in records():
                r = dict(r); r.pop("stdin", None); print("  ", r)
    passed = sum(results.values())
    print(f"\n{passed}/{len(results)} passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
