#!/usr/bin/env python3
"""真 herdr 端到端(09-23):miyu TUI 跑在 herdr 的 pane 里,中转线的 CLI 带着 herdr
钩子,看侧栏最后停在哪、kill 之后还挂不挂着、通知走哪条路。

    BIN=<miyu 二进制> python3 testkit/herdr/real_herdr.py

用户现场:「任务完成了 herdr 还显示进行中,而且没有提示音」。根因是 daemon 由 pane
里的 miyu 拉起、继承了那个 pane 的 `HERDR_PANE_ID`,中转线的 agy/claude/codex 子
进程再继承 daemon——它们装着 herdr 的钩子,一启动就往那个 pane 报「我是这里的官方
会话」,herdr 从此静默丢掉 `custom:miyu` 的所有上报,idle 永远到不了。

这份测具用**真 herdr**(不是 `run.py` 那个假 herdr:假的只会照着我们以为的规则
走,「官方会话占住 pane」这条规则当初就是没人知道才漏的):

- herdr 服务端跑在独立的 `XDG_CONFIG_HOME` 下,有自己的 socket,不碰人正在用的
  那个 herdr;跑测具的进程自己的 `HERDR_*` 一律剥掉再起它。
- pane 里的 shell 固定 bash(用户说「从 bash 打开就有事」)。
- 供应商是假 agy:和真 agy 装的 herdr 钩子一样,环境里有 `HERDR_PANE_ID` 就往那个
  pane 报 `pane.report_agent_session`,然后回一句话。
- 提示音与弹窗:`PATH` 前面垫一层假 `notify-send` / `canberra-gtk-play` / `pw-play`
  / `paplay` / `ffplay`,谁被叫到就记一笔。

判定:
  tui_reported           TUI 起来后侧栏出现 miyu
  relay_cli_no_pane      假 agy 拿不到 HERDR_PANE_ID,也就没去认领 pane
  idle_after_turn        回合结束侧栏回到 idle(被认领时会一直卡在 working)
  popup_via_system       失焦时回合结束:弹窗走 notify-send(herdr 吞 OSC 99)
  sound_left_to_herdr    失焦时回合结束:Miyu 自己一声不响,提示音交给 herdr
  sigterm_releases       对 TUI 发 SIGTERM:进程退出,侧栏那行被释放
一条判定一行 ✅/❌,最后 n/m passed。产物在 ~/.cache/miyu-herdr-real/。

daemon 自己忘没忘坐标这里看不出来(中转线那层把 HERDR_* 全剥了,假 agy 身上分不出
是哪层起的作用;`/proc/<pid>/environ` 是启动那一刻的环境区,删了变量也不变),单独
由 `daemon_env.py` 验。
"""
import json
import os
import shutil
import signal
import socket
import stat
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu").resolve()
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-herdr-real")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
AGY_CONFIG = OUT / "agy-config"
FAKE_AGY = OUT / "fake-agy"
FAKE_BIN = OUT / "fake-bin"
AGY_LOG = OUT / "fake-agy.jsonl"
SOUND_LOG = OUT / "sound.log"
HERDR_ROOT = OUT / "herdr"
PORT = int(os.environ.get("PORT", "18561"))

# 跑测具的进程多半自己就在 herdr 里(AI 会话的 pane),它的 HERDR_* 一个都不能漏进来。
BASE_ENV = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
HERDR_ENV = dict(
    BASE_ENV,
    XDG_CONFIG_HOME=str(HERDR_ROOT / "config"),
    XDG_STATE_HOME=str(HERDR_ROOT / "state"),
    # herdr 自己的提示音关掉:这里只看 Miyu 那边叫没叫播放器。
    HERDR_DISABLE_SOUND="1",
    SHELL="/usr/bin/bash",
    # 下面这些由 pane 里的 bash 继承给 miyu。
    MIYU_HOME=str(HOME),
    XDG_RUNTIME_DIR=str(RUNTIME),
    MIYU_AGY_CONFIG_DIR=str(AGY_CONFIG),
    FAKE_AGY_LOG=str(AGY_LOG),
    FAKE_SOUND_LOG=str(SOUND_LOG),
    PATH=f"{FAKE_BIN}:{BASE_ENV.get('PATH', '')}",
    LANG="zh_CN.UTF-8",
)
SOCKET = HERDR_ROOT / "config" / "herdr" / "herdr.sock"

FAKE_AGY_SOURCE = r'''#!/usr/bin/env python3
import json, os, socket, sys, time
args = sys.argv[1:]
def opt(name):
    return args[args.index(name) + 1] if name in args else None
sid = opt("--conversation") or f"fake-{os.getpid()}"
log = os.environ.get("FAKE_AGY_LOG")
def record(obj):
    with open(log, "a", encoding="utf-8") as f:
        f.write(json.dumps(obj, ensure_ascii=False) + "\n")
def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n"); sys.stdout.flush()
# 和真 agy 的 herdr 钩子(~/.gemini/config/hooks/herdr-agent-state.sh)同一个请求:
# 环境里有 pane 身份就报「这个 pane 是我的会话」。
pane = os.environ.get("HERDR_PANE_ID")
sock = os.environ.get("HERDR_SOCKET_PATH")
claimed = None
if os.environ.get("HERDR_ENV") == "1" and pane and sock:
    request = {"id": "fake-agy", "method": "pane.report_agent_session", "params": {
        "pane_id": pane, "source": "herdr:antigravity_cli", "agent": "agy",
        "seq": time.time_ns(), "agent_session_id": sid}}
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(2)
        client.connect(sock)
        client.sendall((json.dumps(request) + "\n").encode())
        claimed = client.recv(4096).decode().strip()[:200]
        client.close()
    except Exception as error:
        claimed = f"error: {error}"
record({"kind": "start", "pid": os.getpid(), "pane": pane,
        "herdr_vars": sorted(k for k in os.environ if k.startswith("HERDR_")), "claimed": claimed})
usage = {"input_tokens": 10, "output_tokens": 2, "thinking_tokens": 0, "cache_read_tokens": 0, "total_tokens": 12}
# init 里要回显 `--agent`:Miyu 拿它确认人格 agent 真的加载上了,对不上整轮判失败。
emit({"event": "init", "conversation_id": sid, "init": {"model": "m", "cwd": "/", "agent": opt("--agent") or "", "tools": []}})
turn = 0
for line in sys.stdin:
    if not line.strip():
        continue
    turn += 1
    # 慢一点:让侧栏有机会先亮成 working。
    time.sleep(1.0)
    body = f"reply {turn}"
    emit({"event": "step_update", "step_update": {"conversation_id": sid, "step_index": 2 * turn - 1, "state": "DONE", "step_type": "user_input"}})
    emit({"event": "step_update", "step_update": {"conversation_id": sid, "step_index": 2 * turn, "state": "DONE", "step_type": "agent_response", "text_delta": body, "usage": usage}})
    emit({"event": "result", "result": {"conversation_id": sid, "status": "SUCCESS", "response": body, "num_turns": turn, "usage": usage}})
    record({"kind": "turn", "pid": os.getpid(), "turn": turn})
'''

# 假播放器 / 假 notify-send:记下自己被谁、带什么参数叫到。
FAKE_TOOL_SOURCE = r'''#!/bin/sh
printf '%s %s\n' "$(basename "$0")" "$*" >> "$FAKE_SOUND_LOG"
exit 0
'''

results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{('  ' + str(detail)[:300]) if detail else ''}", flush=True)


def herdr(*args, timeout=20):
    return subprocess.run(["herdr", *args], env=HERDR_ENV, capture_output=True, text=True,
                          timeout=timeout)


def herdr_json(*args):
    out = herdr(*args)
    try:
        return json.loads(out.stdout)["result"]
    except Exception:
        return {"_error": (out.stdout + out.stderr)[:300]}


def agent_row(pane):
    for agent in herdr_json("agent", "list").get("agents", []):
        if agent.get("pane_id") == pane:
            return agent
    return None


def agent_status(pane):
    row = agent_row(pane)
    return None if row is None else f"{row.get('agent')}:{row.get('agent_status')}"


def wait_until(predicate, timeout, step=0.3):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return predicate()


def screen(pane):
    return herdr("pane", "read", pane, "--source", "visible").stdout


def agy_records():
    if not AGY_LOG.exists():
        return []
    return [json.loads(line) for line in AGY_LOG.read_text(encoding="utf-8").splitlines() if line.strip()]


def sound_lines():
    if not SOUND_LOG.exists():
        return []
    return [line for line in SOUND_LOG.read_text(encoding="utf-8").splitlines() if line.strip()]


def pid_on_port(port):
    out = subprocess.run(["ss", "-ltnpH", f"sport = :{port}"], capture_output=True, text=True).stdout
    for token in out.replace(",", " ").split():
        if token.startswith("pid="):
            return int(token[4:])
    return None


def environ_of(pid):
    try:
        raw = Path(f"/proc/{pid}/environ").read_bytes()
    except OSError:
        return {}
    pairs = (item.split(b"=", 1) for item in raw.split(b"\0") if b"=" in item)
    return {key.decode(errors="replace"): value.decode(errors="replace") for key, value in pairs}


def tui_pid():
    """pane 里跑着的那个 miyu TUI(不是 daemon)。"""
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            argv = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if not argv or Path(argv[0].decode(errors="replace")).resolve() != BIN:
            continue
        if b"__daemon" in argv:
            continue
        if environ_of(int(entry.name)).get("MIYU_HOME") == str(HOME):
            return int(entry.name)
    return None


def alive(pid):
    try:
        state = Path(f"/proc/{pid}/stat").read_text().split(")")[-1].split()[0]
    except FileNotFoundError:
        return False
    return state != "Z"


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "agy",
        "active_provider_models": [{"provider_id": "agy", "model": "m"}],
        "providers": [{
            "id": "agy", "display_name": "Agy", "base_url": "", "protocol": "antigravity",
            "api_key": "", "models": ["m"], "enabled": True,
        }],
        "memory": {"enabled": False},
        "notifications": {"enabled": True, "on_turn_complete": True, "sound": True},
        "plugins": {"antigravity": {
            "binary": str(FAKE_AGY), "native_tools": "off", "miyu_tools": "off",
            "reuse_process": False,
        }},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2), "utf-8")
    # 客户端自己拉 daemon 时端口取自这份文件;没有它会去撞本机真 daemon 的 8300。
    (HOME / "state").mkdir(parents=True, exist_ok=True)
    (HOME / "state" / "daemon-launch.json").write_text(json.dumps({"port": PORT}), "utf-8")


def install_fakes():
    FAKE_AGY.write_text(FAKE_AGY_SOURCE, "utf-8")
    FAKE_AGY.chmod(FAKE_AGY.stat().st_mode | stat.S_IEXEC)
    FAKE_BIN.mkdir(parents=True, exist_ok=True)
    for name in ("notify-send", "canberra-gtk-play", "pw-play", "paplay", "ffplay", "afplay"):
        path = FAKE_BIN / name
        path.write_text(FAKE_TOOL_SOURCE, "utf-8")
        path.chmod(path.stat().st_mode | stat.S_IEXEC)


def stop_everything(server):
    pid = pid_on_port(PORT)
    if pid:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    herdr("server", "stop")
    try:
        server.wait(timeout=10)
    except subprocess.TimeoutExpired:
        server.kill()


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    if shutil.which("herdr") is None:
        print("herdr 没装,跳过")
        return 0
    if OUT.exists():
        shutil.rmtree(OUT)
    for path in (RUNTIME, AGY_CONFIG, HERDR_ROOT / "config", HERDR_ROOT / "state"):
        path.mkdir(parents=True, exist_ok=True)
    install_fakes()
    write_config()
    server = subprocess.Popen(["herdr", "server"], env=HERDR_ENV, stdin=subprocess.DEVNULL,
                              stdout=(OUT / "herdr-server.out").open("w"), stderr=subprocess.STDOUT)
    try:
        assert wait_until(SOCKET.exists, 15), "herdr server 没起来"
        time.sleep(0.5)
        created = herdr_json("workspace", "create")
        pane = created.get("root_pane", {}).get("pane_id")
        assert pane, f"workspace create: {created}"
        # 等 bash 出提示符再敲命令。
        wait_until(lambda: "$" in screen(pane), 15)
        herdr("pane", "run", pane, f"exec {BIN}")
        row = wait_until(lambda: agent_status(pane), 40)
        check("tui_reported", row and row.startswith("miyu:"), row)
        # 等 TUI 真画出来(大厅)再动手,不然字会被 shell 吃掉。
        time.sleep(3.0)
        herdr("pane", "split", pane, "--direction", "right")
        time.sleep(1.0)

        herdr("pane", "send-text", pane, "hello there")
        time.sleep(0.4)
        herdr("pane", "send-keys", pane, "enter")
        # 人的真实动作:发完消息切去别处。herdr 会给 miyu 那个 pane 发「失焦」
        # (`ESC[O`)——前提是它打开了焦点上报,回合里一定开着。光 split 不挪焦点
        # (实测),得再 focus 一次。假 agy 每轮先睡一秒,来得及。
        time.sleep(0.3)
        herdr("pane", "focus", "--pane", pane, "--direction", "right")
        focused = [p["pane_id"] for p in herdr_json("pane", "list").get("panes", []) if p.get("focused")]
        print(f"   (焦点在: {focused})")
        working = wait_until(lambda: agent_status(pane) == "miyu:working", 20, step=0.1)
        print(f"   (回合中侧栏: {agent_status(pane)}; 亮过 working: {bool(working)})")
        wait_until(lambda: any(r["kind"] == "turn" for r in agy_records()), 60)
        # 回合收尾:idle 上报、通知都是回合结束那一刻发的,给它几秒。
        time.sleep(4.0)

        starts = [r for r in agy_records() if r["kind"] == "start"]
        check("relay_cli_no_pane", starts and all(not r["pane"] for r in starts),
              json.dumps(starts, ensure_ascii=False)[:300])
        final = wait_until(lambda: agent_status(pane) in ("miyu:idle", "miyu:done"), 5)
        check("idle_after_turn", final, agent_status(pane))

        sounds = sound_lines()
        popups = [line for line in sounds if line.startswith("notify-send")]
        players = [line for line in sounds if not line.startswith("notify-send")]
        check("popup_via_system", popups, sounds)
        check("sound_left_to_herdr", not players, players)

        pid = tui_pid()
        if pid is None:
            check("sigterm_releases", False, "找不到 TUI 进程")
        else:
            os.kill(pid, signal.SIGTERM)
            exited = wait_until(lambda: not alive(pid), 10)
            released = wait_until(lambda: agent_row(pane) is None, 5)
            check("sigterm_releases", exited and released,
                  f"exited={exited} row={agent_status(pane)}")
        (OUT / "screen.txt").write_text(screen(pane), "utf-8")
    finally:
        stop_everything(server)
    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({OUT})")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
