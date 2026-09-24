#!/usr/bin/env python3
"""/reset 一族和 /stop 的回执 3 秒后自己撤回（用户 09-24）：沙箱 daemon + 假 NapCat（反向 WS）。

    BIN=<miyu> python3 testkit/qq-reset-recall/run.py

管理员在群里、私聊里发命令，看假 NapCat 收到的 API 调用：回执发出去（send_*）之后，
同一个消息号上有没有 delete_msg、隔了多久。假 NapCat 收到群消息的 delete_msg 后照真
NapCat 的样子推回一条 group_recall 通知，最后停掉 daemon 查消息历史库。不需要模型：
这些命令不进模型。

判定（撤回要在回执发出后 2.8–6 秒之间）：
  group_reset            群里 /reset：回执被撤
  group_reset_usage      群里 /reset now（回用法提示）：一样撤
  group_reset_all_memory 群里 /reset-all-memory：一样撤
  private_reset          私聊 /reset：一样撤
  private_reset_memory   私聊 /reset-memory：一样撤
  group_stop             群里 /stop：一样撤
  group_models_kept      群里 /models：留着
  group_wipe_kept        群里 /wipe（确认提示）：留着
  guest_reset_silent     非管理员群友 /reset：不回话，也就没有可撤的
  history_marks_recall   消息历史里那条 /reset 回执是机器人发的，并且记成了已撤回
  history_keeps_models   /models 的回执在历史里没有撤回记录
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

# 跑测具的进程多半坐在某个 herdr pane 里：HERDR_* 漏给被测的 miyu，它就会往那个 pane
# 报状态（09-23）。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "testkit"))
import sandbox_dir  # noqa: E402

BIN = Path(os.environ["BIN"])
SANDBOX = sandbox_dir.make("miyu-qq-reset-recall-")
HOME = SANDBOX / "home"
RUNTIME = SANDBOX / "runtime"
HISTORY_DB = HOME / "data" / "platforms" / "onebot" / "message_history" / "history.sqlite3"

spec = importlib.util.spec_from_file_location("fake", REPO / "testkit" / "fake-onebot" / "run.py")
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)
ADMIN, GUEST = 810000001, 810000003

RECALL_MIN, RECALL_MAX = 2.8, 6.0
# 不该撤的等多久才算「确实没撤」：比撤回时间上限再多一点。
KEPT_WAIT = RECALL_MAX + 1.0


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


PORT, QQ_PORT = free_port(), free_port()
fake.PORT = QQ_PORT


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        # 命令不进模型；给个连不上的供应商，只为让配置完整。
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-a"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{free_port()}/v1",
            "protocol": "openai-chat", "api_key": "stub", "models": ["stub-a"],
        }],
        "memory": {"enabled": False},
        "platforms": {"qq": {
            "enabled": True, "reverse_ws_port": QQ_PORT, "access_token": "",
            "admin_users": [ADMIN],
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


LOCK = threading.Lock()
SENDS = []    # (message_id, kind, text, answered_at)
KINDS = {}    # message_id -> group / private
DELETES = {}  # message_id -> received_at


def pump(ws):
    while True:
        try:
            frame = ws.recv()
        except Exception as error:
            print(f"  [pump] end: {error}")
            return
        if not isinstance(frame, dict) or "action" not in frame:
            continue
        action, params = frame["action"], frame.get("params", {})
        data = fake.api_data(action, params)
        if action in ("send_group_msg", "send_msg", "send_private_msg"):
            kind = "private" if action == "send_private_msg" or params.get("message_type") == "private" else "group"
            with LOCK:
                SENDS.append((data["message_id"], kind, fake.render(params.get("message")), time.time()))
                KINDS[data["message_id"]] = kind
        ws.send({"status": "ok", "retcode": 0, "data": data, "echo": frame.get("echo")})
        if action == "delete_msg":
            message_id = int(params.get("message_id"))
            with LOCK:
                DELETES[message_id] = time.time()
            # 真 NapCat 撤回自己的群消息也会推 group_recall（发送者、操作者都是自己）。
            if KINDS.get(message_id) == "group":
                ws.send({
                    "post_type": "notice", "notice_type": "group_recall",
                    "self_id": fake.SELF_ID, "group_id": fake.GROUP_ID,
                    "user_id": fake.SELF_ID, "operator_id": fake.SELF_ID,
                    "message_id": message_id, "time": int(time.time()),
                })


def command(send, wait=6.0):
    """发一条命令，等回执；返回 (message_id, 回执文字, 回执发出时刻)，没回就是 None。"""
    with LOCK:
        before = len(SENDS)
    send()
    deadline = time.time() + wait
    while time.time() < deadline:
        with LOCK:
            if len(SENDS) > before:
                message_id, _, text, answered_at = SENDS[before]
                return message_id, text, answered_at
        time.sleep(0.1)
    return None


def main():
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    env = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")
    log_path = SANDBOX / "daemon.log"
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=env, cwd=str(HOME),
                              stdin=subprocess.DEVNULL, stdout=log_path.open("w"), stderr=subprocess.STDOUT)
    results = {}
    receipts = {}
    try:
        assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon not up"
        time.sleep(1.5)
        ws = fake.WS.connect("")
        threading.Thread(target=pump, args=(ws,), daemon=True).start()
        time.sleep(1.5)

        group = lambda text, sender=ADMIN: (lambda: fake.group_msg(ws, text, sender=sender))
        private = lambda text, sender=ADMIN: (lambda: fake.private_msg(ws, text, sender=sender))
        plan = [
            ("group_reset", group("/reset"), True),
            ("group_reset_usage", group("/reset now"), True),
            ("group_reset_all_memory", group("/reset-all-memory"), True),
            ("private_reset", private("/reset"), True),
            ("private_reset_memory", private("/reset-memory"), True),
            ("group_stop", group("/stop"), True),
            ("group_models_kept", group("/models"), False),
            ("group_wipe_kept", group("/wipe"), False),
        ]
        for label, send, _ in plan:
            receipts[label] = command(send)
            shown = receipts[label][1][:40] if receipts[label] else "（没回）"
            print(f"  → {label}: {shown}")
            # 两条命令的消息号是毫秒时间戳，隔开一点免得撞号。
            time.sleep(0.3)
        guest = command(group("/reset", sender=GUEST), wait=3.0)

        last_sent = max(receipt[2] for receipt in receipts.values() if receipt)
        while time.time() < last_sent + KEPT_WAIT:
            time.sleep(0.2)

        with LOCK:
            deletes = dict(DELETES)
        for label, _, recall in plan:
            receipt = receipts[label]
            if not receipt:
                results[label] = False
                print(f"❌ {label}: 没收到回执")
                continue
            message_id, _, answered_at = receipt
            deleted_at = deletes.get(message_id)
            if recall:
                gap = None if deleted_at is None else deleted_at - answered_at
                ok = gap is not None and RECALL_MIN <= gap <= RECALL_MAX
                results[label] = ok
                print(f"{'✅' if ok else '❌'} {label}: {'没撤' if gap is None else f'{gap:.2f}s 后撤回'}")
            else:
                ok = deleted_at is None
                results[label] = ok
                print(f"{'✅' if ok else '❌'} {label}: {'留着' if ok else '被撤了'}")
        results["guest_reset_silent"] = guest is None
        print(f"{'✅' if guest is None else '❌'} guest_reset_silent: {'没回话' if guest is None else '回了话'}")
    finally:
        daemon.terminate()
        try:
            daemon.wait(10)
        except subprocess.TimeoutExpired:
            daemon.kill()

    # daemon 停了才读库（活库只读也要避开）。
    reset_receipt, models_receipt = receipts.get("group_reset"), receipts.get("group_models_kept")
    history_ok = keeps_ok = False
    if HISTORY_DB.exists() and reset_receipt and models_receipt:
        db = sqlite3.connect(f"file:{HISTORY_DB}?mode=ro", uri=True)
        row = db.execute("SELECT is_bot FROM messages WHERE message_id = ?", (str(reset_receipt[0]),)).fetchone()
        recalled = db.execute("SELECT COUNT(*) FROM recalls WHERE message_id = ?", (str(reset_receipt[0]),)).fetchone()[0]
        models_recalled = db.execute("SELECT COUNT(*) FROM recalls WHERE message_id = ?", (str(models_receipt[0]),)).fetchone()[0]
        db.close()
        history_ok = row is not None and row[0] == 1 and recalled == 1
        keeps_ok = models_recalled == 0
        print(f"{'✅' if history_ok else '❌'} history_marks_recall: 回执入库={row is not None} 机器人发的={bool(row and row[0])} 撤回记录={recalled}")
        print(f"{'✅' if keeps_ok else '❌'} history_keeps_models: 撤回记录={models_recalled}")
    else:
        print(f"❌ history: 库不在或缺回执（{HISTORY_DB.exists()=}）")
    results["history_marks_recall"] = history_ok
    results["history_keeps_models"] = keeps_ok

    passed = sum(results.values())
    print(f"{passed}/{len(results)} passed")
    if passed != len(results):
        print("daemon.log 末尾：")
        print("\n".join(log_path.read_text(errors="replace").splitlines()[-30:]))
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
