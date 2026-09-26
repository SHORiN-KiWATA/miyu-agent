#!/usr/bin/env python3
"""QQ 里子代理的汇报(09-26 子代理只在后台跑):沙箱 daemon + 假 NapCat(反向 WS)+ 桩 LLM。

用户拍板:同一轮派出去的几个子代理,等最后一个跑完合成一份再起一轮;账号掉线时汇报留在
会话库里,连上再发(daemon 重启也不丢)。桩模型是 testkit/subagent-session/stub.py。

    BIN=<miyu> python3 testkit/qq-subagent-reports/run.py

判定:
  pair_merged_into_one_wake   管理员在群里让她并排派两个子代理:只起一轮唤醒,那一轮带两份汇报
  pair_reply_carries_both     这一轮说的话里两个子代理的结论都在,群里收到了
  offline_report_is_held      子代理跑完时账号掉线:不起唤醒轮,汇报落在会话库 held_job_reports
  reconnect_delivers_held     连上之后补发:唤醒一轮、群里收到,库里那行删掉
  restart_keeps_held          掉线期间 daemon 重启:汇报还在库里,连上之后照样补发
"""
import importlib.util
import json
import os
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里:HERDR_* 漏给被测的 miyu,它就会往那个 pane
# 报状态(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "testkit"))
import sandbox_dir  # noqa: E402

BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu")
SANDBOX = sandbox_dir.make("miyu-qq-subagent-reports-")
HOME = SANDBOX / "home"
RUNTIME = SANDBOX / "runtime"
STUB_LOG = SANDBOX / "stub.jsonl"

spec = importlib.util.spec_from_file_location("fake", REPO / "testkit" / "fake-onebot" / "run.py")
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)
ADMIN = 810000001


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


PORT, QQ_PORT, STUB_PORT = free_port(), free_port(), free_port()
fake.PORT = QQ_PORT

results = {}


def check(name, ok, detail=""):
    results[name] = bool(ok)
    print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:300]}", flush=True)


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
        "prompt": {"active_persona": ""},
        "platforms": {"qq": {
            "enabled": True, "reverse_ws_port": QQ_PORT, "access_token": "",
            "admin_users": [ADMIN],
            # 桩模型的回复很长(回执 JSON、整段汇报):别转成图、别拆条,好按文字判。
            "max_reply_chars": 0,
            "plugins": {"reply_processor": {"enabled": False}},
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


def wait_for(predicate, timeout, step=0.3):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


LOCK = threading.Lock()
SENDS = []  # 假 NapCat 收到的每条发言(文字)


def pump(ws):
    while True:
        try:
            frame = ws.recv()
        except Exception:
            return
        if not isinstance(frame, dict) or "action" not in frame:
            continue
        action, params = frame["action"], frame.get("params", {})
        data = fake.api_data(action, params)
        if action in ("send_group_msg", "send_msg", "send_private_msg"):
            with LOCK:
                SENDS.append(fake.render(params.get("message")))
        try:
            ws.send({"status": "ok", "retcode": 0, "data": data, "echo": frame.get("echo")})
        except Exception:
            return


def connect():
    ws = fake.WS.connect("")
    threading.Thread(target=pump, args=(ws,), daemon=True).start()
    time.sleep(1.0)
    return ws


def disconnect(ws):
    """真断开:收帧线程还堵在 recv 上时光 close 不会发 FIN,daemon 看不出掉线。"""
    try:
        ws.sock.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass
    ws.sock.close()
    time.sleep(0.5)


def sends_since(mark):
    with LOCK:
        return list(SENDS[mark:])


def said(mark, needle):
    return any(needle in text for text in sends_since(mark))


def wake_requests():
    """桩模型收到的主会话唤醒请求(最后一条是后台汇报)。"""
    if not STUB_LOG.exists():
        return []
    rows = [json.loads(line) for line in STUB_LOG.read_text(encoding="utf-8").splitlines() if line.strip()]
    return [row for row in rows if row["role"] == "main" and row.get("reports")]


def held_rows():
    rows = []
    for db in HOME.rglob("conversation.db"):
        try:
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
            rows += con.execute("SELECT job_id, batch FROM held_job_reports").fetchall()
            con.close()
        except sqlite3.Error:
            pass
    return rows


def start_daemon(tag):
    env = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")
    log = (SANDBOX / f"daemon-{tag}.log").open("w")
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=env, cwd=str(HOME),
                              stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
    assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon not up"
    time.sleep(1.5)
    return daemon


def stop_daemon(daemon):
    daemon.terminate()
    try:
        daemon.wait(15)
    except subprocess.TimeoutExpired:
        daemon.kill()


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    stub_env = dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_LOG=str(STUB_LOG))
    stub = subprocess.Popen([sys.executable, str(REPO / "testkit" / "subagent-session" / "stub.py")], env=stub_env)
    daemon = None
    try:
        daemon = start_daemon("a")
        ws = connect()

        # 1. 同一轮派两个:只起一轮唤醒,带两份汇报。
        mark = len(SENDS)
        fake.group_msg(ws, "TK pair", sender=ADMIN, at_self=True)
        started = wait_for(lambda: said(mark, "PARENT_STARTED_BG"), 30)
        woke = wait_for(lambda: said(mark, "WOKEN:"), 30)
        time.sleep(3.0)  # 要是拆成了两轮,第二轮这时候也该到了
        wakes = wake_requests()
        check("pair_merged_into_one_wake",
              started and woke and [row["reports"] for row in wakes] == [2],
              {"started": bool(started), "woken": bool(woke),
               "reports_per_wake": [row["reports"] for row in wakes]})
        reply = "\n".join(sends_since(mark))
        check("pair_reply_carries_both",
              "reports=2" in reply and "CHILD_RESULT ok" in reply and "CHILD_QUICK ok" in reply,
              {"tail": reply[-240:]})

        # 2. 子代理跑完时账号掉线:汇报留在库里,不起唤醒轮;连上之后补发。
        before = len(wake_requests())
        mark = len(SENDS)
        fake.group_msg(ws, "TK nap", sender=ADMIN, at_self=True)
        wait_for(lambda: said(mark, "PARENT_STARTED_BG"), 30)
        disconnect(ws)
        held = wait_for(held_rows, 15)
        time.sleep(1.0)
        check("offline_report_is_held", held and len(wake_requests()) == before,
              {"held": held, "wake_requests": len(wake_requests()) - before})
        mark = len(SENDS)
        ws = connect()
        delivered = wait_for(lambda: said(mark, "WOKEN:"), 30)
        cleared = wait_for(lambda: not held_rows(), 10)
        check("reconnect_delivers_held",
              delivered and cleared and len(wake_requests()) == before + 1,
              {"delivered": bool(delivered), "held_left": held_rows(), "wake_requests": len(wake_requests()) - before})

        # 3. 掉线期间 daemon 重启:汇报还在库里,连上照样补发。
        before = len(wake_requests())
        mark = len(SENDS)
        fake.group_msg(ws, "TK nap", sender=ADMIN, at_self=True)
        wait_for(lambda: said(mark, "PARENT_STARTED_BG"), 30)
        disconnect(ws)
        held = wait_for(held_rows, 15)
        stop_daemon(daemon)
        daemon = start_daemon("b")
        mark = len(SENDS)
        ws = connect()
        delivered = wait_for(lambda: said(mark, "WOKEN:"), 30)
        check("restart_keeps_held",
              held and delivered and not held_rows() and len(wake_requests()) == before + 1,
              {"held": held, "delivered": bool(delivered), "held_left": held_rows(),
               "wake_requests": len(wake_requests()) - before})
    finally:
        if daemon is not None:
            stop_daemon(daemon)
        stub.terminate()

    passed = sum(results.values())
    print(f"\n{passed}/{len(results)} passed")
    if passed != len(results):
        for log in sorted(SANDBOX.glob("daemon-*.log")):
            print(f"--- {log.name} 末尾 ---")
            print("\n".join(log.read_text(errors="replace").splitlines()[-20:]))
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
