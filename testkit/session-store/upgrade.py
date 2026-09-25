#!/usr/bin/env python3
"""会话项目第 1 段：老数据升级进库、删会话清干净、回退到老版本照样能开库。

    BIN=<新 miyu> OLD_BIN=<老 miyu，可省> python3 testkit/session-store/upgrade.py

沙箱 daemon（隔离 MIYU_HOME，默认端口 18571），不调模型。步骤：
1. 起一次 daemon 建库，另建一个会话（当前那个是终端集成会话，按设计删不掉），停掉；
2. 照老版本的样子给这个会话摆好老文件：待办、思考档位钉、上键历史，以及机器级的
   usage.json、usage-history.jsonl；
3. 再起 daemon，经网页接口读待办、档位钉、用量统计，应当都是老文件里的；老文件
   本版还在；
4. 删掉这个会话：老文件、库里的行都没了；
5. 给了 OLD_BIN（上一版）就拿它起一次 daemon：库版本号没动，老程序照样开得了。
"""
import http.client
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里：HERDR_* 漏给被测的 miyu 会搅乱那个 pane（09-23）。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BIN = Path(os.environ["BIN"])
OLD_BIN = os.environ.get("OLD_BIN")
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-session-store")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18571"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")

RESULTS = []
COOKIE = []


def check(label, ok, detail=""):
    RESULTS.append(ok)
    print(f"{'✅' if ok else '❌'} {label}" + (f"  ({detail})" if detail else ""))


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub", "base_url": "http://127.0.0.1:1/v1",
                       "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"],
                       "default_model": "stub-model"}],
        "memory": {"enabled": False},
        "display": {"language": "zh"},
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


def api(method, path, body=None):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    headers = {"Accept": "application/json", "Origin": BASE}
    if COOKIE:
        headers["Cookie"] = COOKIE[0]
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    conn.request(method, path, body=data, headers=headers)
    response = conn.getresponse()
    set_cookie = response.getheader("Set-Cookie")
    if set_cookie:
        COOKIE[:] = [set_cookie.split(";", 1)[0]]
    raw = response.read()
    conn.close()
    try:
        return response.status, (json.loads(raw) if raw else None)
    except json.JSONDecodeError:
        return response.status, raw.decode(errors="replace")


class Daemon:
    def __init__(self, binary, log):
        self.process = subprocess.Popen([str(binary), "__daemon", "--port", str(PORT)], env=ENV,
                                        cwd=str(HOME), stdin=subprocess.DEVNULL,
                                        stdout=(OUT / log).open("w"), stderr=subprocess.STDOUT)
        self.up = wait_http(f"{BASE}/")
        COOKIE.clear()
        if self.up:
            status, _ = api("POST", "/api/auth/login", {"username": "miyu", "password": "miyu"})
            self.up = status == 204

    def stop(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def conversation_db():
    found = sorted(HOME.rglob("conversation.db"))
    return found[0] if found else None


def db_rows(sql, params=()):
    """只读查沙箱库（`mode=ro`，不碰写锁）。"""
    path = conversation_db()
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    state = HOME / "state"

    daemon = Daemon(BIN, "daemon-1.log")
    try:
        assert daemon.up, "daemon not up"
        status, created = api("POST", "/api/sessions", {"name": "升级前的会话", "switch": False})
        assert status in (200, 201), created
        sid = created["session"]["session_id"]
    finally:
        daemon.stop()

    now = int(time.time())
    legacy = {
        "todos": state / "todos" / f"{sid}.json",
        "pins": state / "session-thinking-variants" / f"{sid}.json",
        "repl": state / "repl-history" / f"{sid}.jsonl",
    }
    for path in legacy.values():
        path.parent.mkdir(parents=True, exist_ok=True)
    legacy["todos"].write_text(json.dumps([{"content": "升级前的待办", "status": "pending", "priority": "high"}],
                                          ensure_ascii=False), "utf-8")
    legacy["pins"].write_text(json.dumps({"selected": {"stub\tstub-model": "high"}}), "utf-8")
    legacy["repl"].write_text('"升级前敲的"\n', "utf-8")
    (state / "usage.json").write_text(json.dumps({"requests": 7, "prompt_tokens": 70, "completion_tokens": 7,
                                                 "total_tokens": 77, "conversation_tokens": 77}), "utf-8")
    (state / "usage-history.jsonl").write_text(
        "".join(json.dumps({"ts": now - offset, "src": "agent", "provider": "stub", "model": "stub-model",
                            "prompt": 10, "completion": 1, "total": 11}) + "\n" for offset in (60, 30)),
        "utf-8")

    daemon = Daemon(BIN, "daemon-2.log")
    try:
        check("升级后 daemon 起得来", daemon.up)
        status, todos = api("GET", f"/api/sessions/{sid}/todos")
        check("待办从老文件导进来了", status == 200 and "升级前的待办" in json.dumps(todos, ensure_ascii=False),
              f"{status} {todos}")
        status, pins = api("GET", f"/api/sessions/{sid}/thinking-variants")
        pinned = (pins or {}).get("pinned", []) if isinstance(pins, dict) else []
        check("档位钉从老文件导进来了", status == 200 and any(item.get("selected") == "high" for item in pinned),
              f"{status} {pins}")
        status, stats = api("GET", "/api/usage/stats?range=all")
        requests = ((stats or {}).get("stats") or {}).get("totals", {}).get("requests") if isinstance(stats, dict) else None
        check("用量明细从老文件导进来了", status == 200 and requests == 2, f"{status} requests={requests}")
        usage_db = state / "usage.db"
        totals = None
        if usage_db.exists():
            conn = sqlite3.connect(f"file:{usage_db}?mode=ro", uri=True)
            row = conn.execute("SELECT state FROM usage_totals WHERE id = 1").fetchone()
            conn.close()
            totals = json.loads(row[0]).get("requests") if row else None
        check("累计从 usage.json 导进来了", totals == 7, f"requests={totals}")
        check("老文件本版还留着", all(path.exists() for path in legacy.values())
              and (state / "usage.json").exists() and (state / "usage-history.jsonl").exists())

        status, body = api("DELETE", f"/api/sessions/{sid}")
        check("删掉老会话", status in (200, 204), f"{status} {str(body)[:160]}")
        left = [name for name, path in legacy.items() if path.exists()]
        check("老会话的老文件一起删了", not left, f"还在: {left}")
        rows = db_rows("SELECT COUNT(*) FROM session_values WHERE session_id = ?", (sid,))[0][0]
        check("库里这个会话的零碎状态也没了", rows == 0, f"rows={rows}")
    finally:
        daemon.stop()

    version = db_rows("PRAGMA user_version")[0][0]
    check("库版本号没动（命名迁移不改 user_version）", version == 40, f"user_version={version}")
    if OLD_BIN:
        old = Daemon(Path(OLD_BIN), "daemon-old.log")
        try:
            check("回退到老版本，daemon 照样开得了库", old.up)
        finally:
            old.stop()
    else:
        print("⏭  没给 OLD_BIN，跳过回退检查")

    print(f"{sum(RESULTS)}/{len(RESULTS)} passed")
    return 0 if all(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
