#!/usr/bin/env python3
"""中转线认单轮覆盖项(09-23):`miyu ask --tools / --no-tools / --no-memory` 在中转线上也生效。

    BIN=<miyu 二进制> python3 testkit/relay-overrides/run.py

中转线(claude-code / codebuddy / codex / agy)的 Miyu 工具只从 MCP 桥拿,桥按会话另建
工具面;覆盖项原来只在回合装配里裁剪,桥看不到——模型照样拿到全部工具,`--no-memory`
的回合还摆着 remember_fact;`--no-tools` 时 CLI 自带的原生工具(Bash/Edit)也照开。

沙箱 daemon + 假 agy(stream-json,同 testkit/agy-reuse 的协议)。假 agy 每轮用自己环境
里的 `MIYU_SESSION` 跑一次 `miyu tool-call --list`——那条路就是桥的目录(同一条
`attach_owner_turn_tools`)——并读 `--agent` 指向的代理文件看原生工具开没开。

判定:
  baseline_bridge_full      不带覆盖项:桥上有 remember_fact 和 run_command,原生工具开着
  allowlist_bridge          --tools read,web_fetch:桥上只剩这两件(至多),原生工具关掉
  no_memory_bridge          --no-memory:桥上没有 remember_fact,别的工具还在,原生工具照开
  no_tools_bridge           --no-tools:桥不挂(或挂了也是空的),原生工具关掉
  restricted_not_resumed    同一会话带 --tools 那一轮不续上前面不带限制的那条 agy 会话、不借它的进程
  plain_still_resumes       同一会话再回到不带覆盖项:接着原来那条(同进程或 --conversation 续上)
一条判定一行 ✅/❌,最后 n/m passed。产物 ~/.cache/miyu-relay-overrides/。
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
BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu").resolve()
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-relay-overrides")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
AGY_CONFIG = OUT / "agy-config"
FAKE = OUT / "fake-agy"
LOG = OUT / "fake-agy.jsonl"
PORT = int(os.environ.get("PORT", "18583"))
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME),
           MIYU_AGY_CONFIG_DIR=str(AGY_CONFIG), FAKE_AGY_LOG=str(LOG), FAKE_MIYU_BIN=str(BIN),
           MIYU_LOG=os.environ.get("MIYU_LOG", "info"))

FAKE_AGY = r'''#!/usr/bin/env python3
import json, os, subprocess, sys, time
from pathlib import Path
args = sys.argv[1:]
def opt(name):
    return args[args.index(name) + 1] if name in args else None
agent = opt("--agent") or ""
resumed = opt("--conversation")
sid = resumed or f"fake-{os.getpid()}-{int(time.time() * 1000) % 100000}"
log = os.environ["FAKE_AGY_LOG"]
def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n"); sys.stdout.flush()
def record(obj):
    with open(log, "a", encoding="utf-8") as f:
        f.write(json.dumps(obj, ensure_ascii=False) + "\n")
agent_file = Path(os.environ["MIYU_AGY_CONFIG_DIR"]) / "agents" / agent / "agent.md"
try:
    agent_text = agent_file.read_text(encoding="utf-8")
    native = "tools: []" not in agent_text.split("---\n\n", 1)[0]
except OSError:
    native = None
def bridge_names():
    # 桥的会话身份就是这个变量(relay_env);没有它 = 桥没挂。
    if not os.environ.get("MIYU_SESSION"):
        return None
    listed = subprocess.run([os.environ["FAKE_MIYU_BIN"], "tool-call", "--list"],
                            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
    return sorted(line.split("\t")[0].strip() for line in listed.stdout.splitlines() if line.strip())
usage = {"input_tokens": 10, "output_tokens": 2, "thinking_tokens": 0, "cache_read_tokens": 0, "total_tokens": 12}
emit({"event": "init", "conversation_id": sid, "init": {"model": "m", "cwd": "/", "agent": agent, "tools": []}})
record({"kind": "start", "pid": os.getpid(), "sid": sid, "resumed": resumed, "native": native,
        "session": os.environ.get("MIYU_SESSION")})
turn = 0
for line in sys.stdin:
    if not line.strip():
        continue
    turn += 1
    names = bridge_names()
    body = f"reply {turn} from pid {os.getpid()}"
    emit({"event": "step_update", "step_update": {"conversation_id": sid, "step_index": 2 * turn - 1, "state": "DONE", "step_type": "user_input"}})
    emit({"event": "step_update", "step_update": {"conversation_id": sid, "step_index": 2 * turn, "state": "DONE", "step_type": "agent_response", "text_delta": body, "usage": usage}})
    emit({"event": "result", "result": {"conversation_id": sid, "status": "SUCCESS", "response": body, "num_turns": turn, "usage": usage}})
    record({"kind": "turn", "pid": os.getpid(), "sid": sid, "turn": turn, "bridge": names, "native": native,
            "session": os.environ.get("MIYU_SESSION")})
'''

results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{('  ' + str(detail)[:320]) if detail else ''}", flush=True)


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "agy",
        "active_provider_models": [{"provider_id": "agy", "model": "m"}],
        "providers": [{
            "id": "agy", "display_name": "Agy", "base_url": "", "protocol": "antigravity",
            "api_key": "", "models": ["m"], "enabled": True,
        }],
        "memory": {"enabled": True},
        "plugins": {"antigravity": {
            "binary": str(FAKE), "native_tools": "all", "miyu_tools": "all",
            "reuse_process": True, "reuse_idle_seconds": 30,
        }},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2), "utf-8")


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


def records():
    if not LOG.exists():
        return []
    return [json.loads(line) for line in LOG.read_text(encoding="utf-8").splitlines() if line.strip()]


def ask(session, text, *flags, create=False):
    """跑一轮,返回这一轮新增的假 agy 记录(start 若有 + turn)。"""
    before = len(records())
    args = [str(BIN), "ask", "--output-format", "json", "--session", session, *flags]
    if create:
        args.append("--create")
    # stdin 必须显式给 /dev/null:CLI 会探测管道 stdin 最多等 5 秒。
    done = subprocess.run([*args, text], env=ENV, stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=120)
    (OUT / f"ask-{session}-{len(records())}.txt").write_text(done.stdout + "\n---\n" + done.stderr, "utf-8")
    new = records()[before:]
    return done.returncode, new


def turn_of(new):
    turns = [r for r in new if r["kind"] == "turn"]
    return turns[-1] if turns else None


def start_of(new):
    starts = [r for r in new if r["kind"] == "start"]
    return starts[-1] if starts else None


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    if OUT.exists():
        shutil.rmtree(OUT)
    for path in (RUNTIME, AGY_CONFIG):
        path.mkdir(parents=True, exist_ok=True)
    FAKE.write_text(FAKE_AGY, "utf-8")
    FAKE.chmod(FAKE.stat().st_mode | stat.S_IEXEC)
    write_config()
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                              stdin=subprocess.DEVNULL,
                              stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    try:
        assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon 没起来(看 daemon.log)"
        time.sleep(1.0)

        code, new = ask("base", "第一句", create=True)
        base = turn_of(new)
        base_start = start_of(new)
        names = (base or {}).get("bridge") or []
        check("baseline_bridge_full",
              code == 0 and "remember_fact" in names and "run_command" in names and (base or {}).get("native") is True,
              f"code={code} native={(base or {}).get('native')} bridge={len(names)} 件")

        code, new = ask("allow", "第二句", "--tools", "read,web_fetch", create=True)
        turn = turn_of(new) or {}
        names = turn.get("bridge")
        check("allowlist_bridge",
              code == 0 and names is not None and names and set(names) <= {"read", "web_fetch"}
              and turn.get("native") is False,
              f"code={code} native={turn.get('native')} bridge={names if names is None or len(names) < 12 else f'{len(names)} 件'}")

        code, new = ask("nomem", "第三句", "--no-memory", create=True)
        turn = turn_of(new) or {}
        names = turn.get("bridge") or []
        check("no_memory_bridge",
              code == 0 and "remember_fact" not in names and "run_command" in names and turn.get("native") is True,
              f"code={code} native={turn.get('native')} remember_fact={'remember_fact' in names} bridge={len(names)} 件")

        code, new = ask("none", "第四句", "--no-tools", create=True)
        turn = turn_of(new) or {}
        names = turn.get("bridge")
        check("no_tools_bridge",
              code == 0 and not names and turn.get("native") is False,
              f"code={code} native={turn.get('native')} bridge={names if names is None or len(names) < 12 else f'{len(names)} 件'}")

        # 同一会话:先带白名单(工具面变了),再回到不带覆盖项。
        code, new = ask("base", "第五句", "--tools", "read")
        restricted_start = start_of(new)
        restricted_turn = turn_of(new) or {}
        not_resumed = (
            code == 0
            and restricted_start is not None
            and restricted_start.get("resumed") != (base_start or {}).get("sid")
            and restricted_turn.get("pid") != (base or {}).get("pid")
        )
        check("restricted_not_resumed", not_resumed,
              f"code={code} start={restricted_start} turn_pid={restricted_turn.get('pid')} base_pid={(base or {}).get('pid')}")

        code, new = ask("base", "第六句")
        plain_turn = turn_of(new) or {}
        plain_start = start_of(new)
        continued = code == 0 and (
            (plain_start is None and plain_turn.get("pid") == (base or {}).get("pid"))
            or (plain_start is not None and plain_start.get("resumed") == (base_start or {}).get("sid"))
        )
        check("plain_still_resumes", continued,
              f"code={code} start={plain_start} turn_pid={plain_turn.get('pid')} base_pid={(base or {}).get('pid')}")
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=15)
        except subprocess.TimeoutExpired:
            daemon.kill()
    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({OUT})")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
